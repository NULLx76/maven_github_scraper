use clap::builder::Str;
use clap::Parser;
use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::owo_colors::OwoColorize;
use color_eyre::Result;
use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use std::collections::{BinaryHeap, HashSet};
use std::fs::remove_file;
use std::io::Stdout;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use std::{
    error::Error,
    fs,
    fs::{DirEntry, File},
    io,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Instant,
};

use dashmap::DashMap;
use rand::prelude::SliceRandom;
use rand::rngs::ThreadRng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use rayon::prelude::IntoParallelRefIterator;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

mod pom;
use pom::{Pom, Repositories};

#[derive(Parser)]
struct Args {
    #[arg(default_value = "../data/latest/data")]
    path: PathBuf,

    /// Build effective pom to analyze
    /// WARNING: Takes much longer
    #[arg(long)]
    effective: bool,
    // /// Sample N random repos instead of all
    // /// This is seeded
    // #[arg(long)]
    // sample: Option<usize>,
}

const EFFECTIVE_FILE_NAME: &str = "effective.xml";
const SEED: [u8; 32] = [42; 32];

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub has_pom: bool,
}

pub fn create_sample_dir(
    full_data_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
    number: usize,
    seed: [u8; 32],
) {
    let full_data_dir = full_data_dir.as_ref();
    let output = output.as_ref();
    fs::create_dir_all(output.join("poms")).unwrap();

    let mut rng = ChaCha20Rng::from_seed(seed);

    let mut reader = csv::Reader::from_path(full_data_dir.join("github.csv")).unwrap();

    let mut repos: Vec<Repo> = reader.deserialize().map(|el| el.unwrap()).collect();
    repos.shuffle(&mut rng);

    repos.truncate(number);

    let mut writer = csv::Writer::from_path(output.join("github.csv")).unwrap();
    for repo in repos {
        let repo_path = repo.name.replace('/', ".");
        let path = full_data_dir.join("poms").join(&repo_path);

        if path.exists() {
            symlink(path, output.join("poms").join(&repo_path)).unwrap();
        }
        writer.serialize(&repo).unwrap();
    }

    writer.flush().unwrap();
}

fn main() {
    create_sample_dir(
        "/home/vivian/src/research_project/data/latest/data",
        "/home/vivian/src/research_project/data/sample40_000",
        40_000,
        SEED,
    );
    return;

    let args = Args::parse();

    if args.effective {
        println!("Parsing effective poms");
    }

    // let mut rng = ChaCha20Rng::from_seed(SEED);

    let repos: DashMap<String, usize> = DashMap::new();
    let distros: DashMap<String, usize> = DashMap::new();
    let counter = AtomicUsize::new(0);
    let errors = Mutex::new(Vec::new());

    let now = Instant::now();

    let dir = args.path.read_dir().unwrap();
    let projects: Vec<PathBuf> = dir
        .par_bridge()
        .filter_map(|d| d.ok().map(|d| d.path()))
        .collect();

    // projects.shuffle(&mut rng);

    // dbg!(projects.len());
    println!("Searching {} projects", projects.len());

    projects
        .par_iter()
        .map(|dir| -> Result<()> {
            let proj = process_folder(dir.as_path(), args.effective)?;

            for repo in proj.repos {
                repos.entry(repo).and_modify(|el| *el += 1).or_insert(1);
            }

            for repo in proj.dist_repos {
                distros.entry(repo).and_modify(|el| *el += 1).or_insert(1);
            }

            counter.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .for_each(|res| {
            if let Err(err) = res {
                errors.lock().unwrap().push(err);
            }
        });

    let duration = Instant::now().duration_since(now);

    let repos_len: usize = repos.iter().map(|el| *el.value()).sum();
    let distros_len: usize = distros.iter().map(|el| *el.value()).sum();
    let repos_top = biggest_n(repos, 5);
    let distros_top = biggest_n(distros, 5);

    println!(
        "Took {} seconds to parse {counter:?} POMs",
        duration.as_secs()
    );

    println!("Found {repos_len} repos. Most used repos are {repos_top:#?}");
    println!("Found {distros_len} distribution repos. Most used repos are {distros_top:#?}");

    fs::write("./errors", format!("{:#?}", errors.lock().unwrap())).unwrap()
}

fn biggest_n(map: DashMap<String, usize>, n: usize) -> Vec<(String, usize)> {
    let mut top: Vec<(String, usize)> = map.into_iter().collect();
    top.sort_by(|(_, a), (_, b)| a.cmp(b).reverse());
    top.truncate(n);

    top
}

#[derive(Debug, Clone)]
struct Project {
    pub repos: HashSet<String>,
    pub dist_repos: HashSet<String>,
}

fn process_folder(path: &Path, build_effective: bool) -> Result<Project> {
    let iter = WalkDir::new(path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| {
            e.ok()
                .and_then(|d| (d.file_name() == "pom.xml").then_some(d.into_path()))
        });

    let mut repos = HashSet::new();
    let mut dist_repos = HashSet::new();

    for mut pom in iter {
        let data = if build_effective {
            pom.set_file_name("effective.xml");
            if pom.exists() {
                let f = File::open(pom)?;
                serde_xml_rs::from_reader(f)?
            } else {
                match effective_pom(pom.parent().unwrap()) {
                    Ok(p) => p,
                    Err(_) => {
                        pom.set_file_name("pom.xml");
                        let f = File::open(pom)?;
                        serde_xml_rs::from_reader(f)?
                    }
                }
            }
        } else {
            pom.set_file_name("pom.xml");
            let f = File::open(pom)?;
            serde_xml_rs::from_reader(f)?
        };

        if let Some(reps) = data.repositories() {
            for repo in reps {
                repos.insert(repo.to_string());
            }
        }

        if let Some(repos) = data.distribution_repositories() {
            for repo in repos {
                dist_repos.insert(repo.to_string());
            }
        }
    }

    Ok(Project { repos, dist_repos })
}

fn effective_pom(path: &Path) -> color_eyre::Result<Pom> {
    let cmd = Command::new("mvn")
        .args([
            "help:effective-pom",
            &format!("-Doutput={EFFECTIVE_FILE_NAME}"),
        ])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .wrap_err("Failed running maven")?;

    if cmd.success() {
        let f = File::open(path.join(EFFECTIVE_FILE_NAME))?;
        let pom = serde_xml_rs::from_reader(f)?;

        Ok(pom)
    } else {
        Err(eyre!("Maven command failed"))
    }
}

#[cfg(test)]
mod tests {
    use crate::process_folder;
    use rayon::iter::IntoParallelRefIterator;
    use rayon::iter::ParallelIterator;
    use std::path::PathBuf;

    #[test]
    fn case() {
        let paths = [
            "../output/18965050.storm-applied",
            "../output/189764.OneArmBandit",
            "../output/189811.zzpj",
            "../output/18981119465.likwid-java-api",
            "../output/18AkbarA.MCQuizbowl",
            "../output/18F.e-manifest-spring",
            "../output/18F.identity-saml-java",
            "../output/18PatZ.PvPStats",
            "../output/18PatZ.Triangulus",
            "../output/18PatZ.files",
        ];

        paths.par_iter().for_each(|el| {
            process_folder(PathBuf::from(el).as_path()).unwrap();
        });
    }
}
