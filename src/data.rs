use crate::{CsvRepo, Repo};
use color_eyre::eyre::Context;
use indicatif::ProgressBar;
use rayon::iter::{ParallelBridge, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::{fs, io};
use thiserror::Error;
use tokio::task::spawn_blocking;
use tracing::info;

#[derive(Debug, Clone)]
pub struct Data {
    pom_dir: PathBuf,
    github_csv: PathBuf,
    fetched: PathBuf,

    state_cache: Arc<AtomicUsize>,
    state_path: PathBuf,
    state_file_lock: Arc<Mutex<()>>,

    csv_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO Error occurred")]
    IO(#[from] io::Error),
    #[error("Serialization")]
    Serde(#[from] serde_json::Error),
    #[error("invalid path")]
    InvalidPath(String),
    #[error("error accessing csv file")]
    Csv(#[from] csv::Error),
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    last_id: Forges,
}

#[derive(Debug, Serialize, Deserialize)]
struct Forges {
    github: usize,
}

impl Data {
    pub async fn new(base_dir: &Path) -> Result<Self, Error> {
        if !base_dir.exists() {
            tokio::fs::create_dir_all(base_dir).await?;
        }
        let state_path = base_dir.join("state.json");
        let state_cache = Arc::new(AtomicUsize::new(0));
        if state_path.exists() {
            let data = tokio::fs::read(&state_path).await?;
            let state: State = serde_json::from_slice(&data)?;
            state_cache.store(state.last_id.github, Ordering::SeqCst);
        }

        let fetched = base_dir.join("fetched");
        if !fetched.exists() {
            tokio::fs::File::create(&fetched).await?;
        }

        Ok(Self {
            pom_dir: base_dir.join("poms"),
            github_csv: base_dir.join("github.csv"),
            fetched,
            state_file_lock: Default::default(),
            state_path,
            state_cache,
            csv_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn get_pom_path(&self, repo: &Repo, path: &str) -> PathBuf {
        self.pom_dir.join(repo.path()).join(path)
    }

    pub async fn write_pom(&self, repo: &Repo, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let file_path = self.get_pom_path(repo, path);
        let parent = file_path
            .parent()
            .ok_or_else(|| Error::InvalidPath("No Parent".to_string()))?;
        tokio::fs::create_dir_all(parent).await?;

        let mut f = File::create(file_path)?;
        f.write_all(bytes)?;

        Ok(())
    }

    pub fn get_last_id(&self) -> Result<usize, Error> {
        Ok(self.state_cache.load(Ordering::SeqCst))
    }

    pub async fn set_last_id(&self, id: usize) -> Result<(), Error> {
        self.state_cache.store(id, Ordering::SeqCst);

        let lock = self.state_file_lock.clone();
        let state_path = self.state_path.clone();
        spawn_blocking(move || -> Result<(), Error> {
            let guard = lock.lock().unwrap();

            let file = File::create(state_path)?;
            let mut file = BufWriter::new(file);
            serde_json::to_writer_pretty(
                &mut file,
                &State {
                    last_id: Forges { github: id },
                },
            )?;
            file.write_all(&[b'\n'])?;

            drop(guard);

            Ok(())
        })
        .await
        .unwrap()?;

        Ok(())
    }

    pub async fn store_repo(&self, repo: CsvRepo) -> Result<(), Error> {
        let lock = self.csv_lock.clone();
        let github_csv = self.github_csv.clone();
        spawn_blocking(move || -> Result<(), Error> {
            let guard = lock.lock().unwrap();

            let mut csv = if github_csv.exists() {
                let file = OpenOptions::new().append(true).open(&github_csv)?;
                csv::WriterBuilder::new()
                    .has_headers(false)
                    .from_writer(file)
            } else {
                let file = File::create(&github_csv)?;
                csv::WriterBuilder::new()
                    .has_headers(true)
                    .from_writer(file)
            };

            csv.serialize(repo)?;

            drop(guard);

            Ok(())
        })
        .await
        .unwrap()?;
        Ok(())
    }

    pub async fn get_non_fetched_repos(&self) -> Result<Vec<CsvRepo>, Error> {
        let fetched = self.fetched.clone();
        let github_csv = self.github_csv.clone();
        spawn_blocking(move || -> Result<Vec<CsvRepo>, Error> {
            let done_str = fs::read_to_string(fetched)?;
            let done: HashSet<_> = done_str.lines().collect();

            let mut rdr = csv::Reader::from_path(github_csv)?;
            let mut repos = Vec::new();

            for record in rdr.deserialize() {
                let record: CsvRepo = record?;
                if !done.contains(record.id.as_str()) {
                    repos.push(record);
                }
            }

            Ok(repos)
        })
        .await
        .unwrap()
    }

    pub async fn mark_fetched(&self, repo: &Repo) -> Result<(), Error> {
        let fetched = self.fetched.clone();
        let id = repo.id.clone();
        spawn_blocking(move || -> Result<(), Error> {
            let mut f = OpenOptions::new().append(true).open(&fetched)?;
            f.write_all(id.as_bytes())?;
            f.write_all("\n".as_bytes())?;

            Ok(())
        })
        .await
        .unwrap()
    }

    pub async fn update_csv_has_pom(&self) -> Result<(), Error> {
        info!("Updating csv from filesystem");
        let csv = self.github_csv.clone();
        let mut new_csv = self.github_csv.clone();
        new_csv.set_extension("csv.new");
        if new_csv.exists() {
            tokio::fs::remove_file(&new_csv).await?;
        }
        let dirs: HashSet<String> = self
            .get_project_dirs()
            .await?
            .into_iter()
            .map(|el| el.to_string_lossy().to_string())
            .collect();

        let spinner = ProgressBar::new(dirs.len() as u64);

        info!("Fetched all dirs");

        let new_path = new_csv.clone();
        spawn_blocking(move || -> Result<(), Error> {
            let mut rdr = csv::Reader::from_path(&csv)?;
            let mut wtr = csv::WriterBuilder::new()
                .has_headers(true)
                .from_path(new_path)?;

            for record in rdr.deserialize() {
                spinner.tick();
                let mut csv_record: CsvRepo = record?;
                let path = csv_record.name.replace('/', ".");
                csv_record.has_pom = csv_record.has_pom || dirs.contains(&path);
                if csv_record.has_pom {
                    spinner.inc(1);
                }

                wtr.serialize(csv_record)?;
            }

            spinner.finish();

            Ok(())
        })
        .await
        .unwrap()?;

        tokio::fs::rename(new_csv, &self.github_csv).await?;

        info!("consolidated CSV successfully");

        Ok(())
    }

    pub async fn get_project_dirs(&self) -> Result<Vec<PathBuf>, Error> {
        let dir = self.pom_dir.read_dir()?;
        let (send, recv) = tokio::sync::oneshot::channel();

        rayon::spawn(move || {
            let projects = dir
                .par_bridge()
                .filter_map(|d| d.ok().map(|d| d.path()))
                .collect();

            send.send(projects).unwrap();
        });

        let projects = recv.await.expect("Rayon panicked");

        Ok(projects)
    }
}
