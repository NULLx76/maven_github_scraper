use crate::Repo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::fs;
use tokio::io::BufWriter;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct Data {
    data_dir: PathBuf,
    pom_dir: PathBuf,
    github_csv: PathBuf,
    fetched: PathBuf,

    state_cache: AtomicUsize,
    state_path: PathBuf,
    state_file_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO Error occurred")]
    IOError(#[from] io::Error),
    #[error("invalid path")]
    InvalidPath(String),
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
            fs::create_dir_all(base_dir).await?;
        }
        let state_path = base_dir.join("state.json");
        let state_cache = AtomicUsize::new(0);
        if state_path.exists() {
            let data = fs::read(&state_path).await?;
            let state: State = serde_json::from_slice(&data)?;
            state_cache.store(state.last_id.github, Ordering::SeqCst);
        }

        Ok(Self {
            data_dir: base_dir.to_path_buf(),
            pom_dir: base_dir.join("poms"),
            github_csv: base_dir.join("github.csv"),
            fetched: base_dir.join("fetched"),
            state_file_lock: Default::default(),
            state_path,
            state_cache,
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
        fs::create_dir_all(parent).await?;

        let mut f = File::create(file_path)?;
        f.write_all(bytes)?;

        Ok(())
    }

    pub fn get_last_id(&self) -> Result<usize, Error> {
        Ok(self.state_cache.load(Ordering::SeqCst))
    }

    pub async fn set_last_id(&self, id: usize) -> Result<JoinHandle<Result<(), Error>>, Error> {
        self.state_cache.store(id, Ordering::SeqCst);

        let handle = tokio::task::spawn(async {
            let guard = self.state_file_lock.lock().unwrap();

            let mut file = BufWriter::new(File::create(&self.state_path)?);
            serde_json::to_writer_pretty(
                &mut file,
                &State {
                    last_id: Forges { github: id },
                },
            )?;
            file.write_all(&[b'\n']).await?;

            drop(guard);

            Ok(())
        });

        // TODO: Await handle here?

        Ok(handle)
    }
}
