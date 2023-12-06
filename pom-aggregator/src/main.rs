use clap::Parser;
use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::Result;
use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use std::collections::HashSet;
use std::fs::remove_file;
use std::io::Stdout;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{
    error::Error,
    fs::{DirEntry, File},
    io,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Instant,
};

use dashmap::DashMap;
use walkdir::WalkDir;

mod pom;
use pom::{Pom, Repositories};

#[derive(Parser)]
struct Args {
    #[arg(default_value = "../output")]
    path: PathBuf,

    /// Build effective pom to analyze
    /// WARNING: Takes much longer
    #[arg(short, long)]
    effective: bool,
}

const EFFECTIVE_FILE_NAME: &str = "effective.xml";

fn main() {
    let args = Args::parse();

    // let repos = DashMap::new();
    // let distros = DashMap::new();
    let counter = AtomicUsize::new(0);
    let dist_repos = AtomicUsize::new(0);
    let repos = AtomicUsize::new(0);

    let now = Instant::now();

    let dir = args.path.read_dir().unwrap();
    dir.par_bridge()
        .map(|el| -> Result<()> {
            let dir = el?;
            let proj = process_folder(dir.path().as_path(), args.effective)?;
            repos.fetch_add(proj.repos.len(), Ordering::Relaxed);
            dist_repos.fetch_add(proj.dist_repos.len(), Ordering::Relaxed);

            counter.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .for_each(|res| {
            if let Err(err) = res {
                let str = format!("{err:?}");
                if !str.contains("Maven") {
                    eprintln!("{str}");
                }
            }
        });

    let duration = Instant::now().duration_since(now);

    println!(
        "Repos {}, Dist Repos {}",
        repos.load(Ordering::SeqCst),
        dist_repos.load(Ordering::SeqCst)
    );

    println!(
        "Took {} seconds to parse {counter:?} POMs",
        duration.as_secs()
    );
}

#[derive(Debug, Clone)]
struct Project {
    repos: HashSet<String>,
    dist_repos: HashSet<String>,
}

fn process_folder(path: &Path, build_effective: bool) -> Result<Project> {
    let iter = WalkDir::new(path).into_iter().filter_map(|e| {
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
