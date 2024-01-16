use crate::data;
use crate::data::Data;
use color_eyre::eyre::{eyre, WrapErr};
use dashmap::DashMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::{fs, io};
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct Pom {
    pub repositories: Option<Repositories>,
    #[serde(rename = "distributionManagement")]
    pub distribution_management: Option<Repositories>,
}

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct Repositories {
    #[serde(rename = "repository", default)]
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Repository {
    pub id: String,
    pub url: String,
}

impl Pom {
    pub fn repositories(&self) -> Option<Vec<&str>> {
        self.repositories.as_ref().map(|repos| {
            repos
                .repositories
                .iter()
                .map(|repo| repo.url.as_str())
                .collect()
        })
    }

    pub fn distribution_repositories(&self) -> Option<Vec<&str>> {
        self.distribution_management.as_ref().map(|repos| {
            repos
                .repositories
                .iter()
                .map(|repo| repo.url.as_str())
                .collect()
        })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Data error: {0:?}")]
    Data(#[from] data::Error),

    #[error("IO Error: {0:?}")]
    IO(#[from] io::Error),
}

fn biggest_n(map: DashMap<String, usize>, n: usize) -> Vec<(String, usize)> {
    let mut top: Vec<(String, usize)> = map.into_iter().collect();
    top.sort_by(|(_, a), (_, b)| a.cmp(b).reverse());
    top.truncate(n);

    top
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub distros: DashMap<String, usize>,
    pub external_repos: DashMap<String, usize>,
    pub has_external_repos: usize,
    pub has_distro_repos: Vec<String>,
    pub errors: Vec<String>,
    pub total: usize,
}

impl Report {
    pub fn print(&self) {
        println!("Found a total of {} repos", self.total);
        println!(
            "Amount of repos with external repos: {}",
            self.has_external_repos
        );
        println!(
            "Amount of repos with distribution repos: {}",
            self.has_distro_repos.len()
        );

        let repos_len = self.external_repos.len();
        let distros_len = self.distros.len();
        let top_repos = biggest_n(self.external_repos.clone(), 25);
        let top_distros = biggest_n(self.distros.clone(), 25);

        println!("Found {repos_len} distinct external repositories, top 25: {top_repos:#?}");
        println!(
            "Found {distros_len} distinct distribution repositories, top 25: {top_distros:#?}"
        );

        fs::write("./analyzer_error_log", format!("{:#?}", self.errors)).unwrap();
    }
}

pub async fn analyze(data: Data, build_effective: bool) -> Result<Report, Error> {
    let projects = data.get_project_dirs().await?;
    let (send, recv) = tokio::sync::oneshot::channel();

    rayon::spawn(move || {
        let distros: DashMap<String, usize> = DashMap::new();
        let repos: DashMap<String, usize> = DashMap::new();
        let has_external_repo = AtomicUsize::new(0);
        let has_distro_repo = Mutex::new(Vec::new());
        let total = AtomicUsize::new(0);
        let errors = Mutex::new(Vec::new());

        projects
            .par_iter()
            .filter_map(|dir| match process_folder(dir, build_effective) {
                Ok(project) => Some(project),
                Err(error) => {
                    errors.lock().unwrap().push(format!("{error:?}"));
                    None
                }
            })
            .for_each(|proj| {
                if !proj.repos.is_empty() {
                    has_external_repo.fetch_add(1, Ordering::SeqCst);
                }

                if !proj.dist_repos.is_empty() {
                    has_distro_repo.lock().unwrap().push(proj.name);
                }

                for repo in proj.repos {
                    repos.entry(repo).and_modify(|el| *el += 1).or_insert(1);
                }

                for repo in proj.dist_repos {
                    distros.entry(repo).and_modify(|el| *el += 1).or_insert(1);
                }

                total.fetch_add(1, Ordering::SeqCst);
            });

        send.send(Report {
            distros,
            external_repos: repos,
            has_external_repos: has_external_repo.load(Ordering::SeqCst),
            has_distro_repos: has_distro_repo.lock().unwrap().clone(),
            errors: errors.lock().unwrap().clone(),
            total: total.load(Ordering::SeqCst),
        })
        .unwrap();
    });

    let data = recv.await.unwrap();

    Ok(data)
}

#[derive(Debug, Clone)]
struct Project {
    pub name: String,
    pub repos: HashSet<String>,
    pub dist_repos: HashSet<String>,
}

const EFFECTIVE_FILE_NAME: &str = "effective.xml";

fn process_folder(path: &Path, build_effective: bool) -> color_eyre::Result<Project> {
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

    let name = path.file_name().unwrap().to_string_lossy().to_string();
    Ok(Project {
        name,
        repos,
        dist_repos,
    })
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
