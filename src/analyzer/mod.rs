use crate::data;
use crate::data::Data;
use color_eyre::eyre::{eyre, WrapErr};
use dashmap::DashMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use std::{fs, io};
use thiserror::Error;
use tokio::time::Instant;
use tracing::{error, info, trace};
use url::Url;
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Report {
    pub distros: DashMap<String, usize>,
    pub external_repos: DashMap<String, usize>,
    pub has_external_repos: usize,
    pub has_distro_repos: Vec<String>,
    pub errors: Vec<String>,
    pub total: usize,
}

pub fn distinct_repos_per_hostname(map: DashMap<String, usize>) {
    // HashMap of HostName to HashSet
    let dashmap: DashMap<_, HashSet<String>> = DashMap::new();

    map.par_iter().for_each(|el| {
        if let Ok(Some(val)) = Url::parse(el.key()).map(|el| el.host().map(|el| el.to_string())) {
            dashmap
                .entry(val)
                .and_modify(|hs| {
                    hs.insert(el.key().to_string());
                })
                .or_insert(HashSet::from([el.key().to_string()]));
        }
    });

    let result: HashMap<_, usize> = dashmap
        .par_iter()
        .map(|el| (el.key().clone(), el.value().len()))
        .collect();

    let json = serde_json::to_string_pretty(&result).unwrap();

    println!("{json}")
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

        println!("{} errors occurred", self.errors.len())

        // fs::write("./analyzer_error_log", format!("{:#?}", self.errors)).unwrap();
    }
}

pub fn most_popular_hostnames(data: Data) -> Result<(), Error> {
    let report = data.read_report()?;
    let distro_hostnames = DashMap::new();
    report.distros.par_iter().for_each(|entry| {
        if let Ok(url) = Url::parse(entry.key()) {
            if let Some(host) = url.host_str() {
                distro_hostnames
                    .entry(host.to_string())
                    .and_modify(|el| *el += entry.value())
                    .or_insert(*entry.value());
            }
        }
    });

    let external_repo_hostnames = DashMap::new();
    report.external_repos.par_iter().for_each(|entry| {
        if let Ok(url) = Url::parse(entry.key()) {
            if let Some(host) = url.host_str() {
                external_repo_hostnames
                    .entry(host.to_string())
                    .and_modify(|el| *el += entry.value())
                    .or_insert(*entry.value());
            }
        }
    });

    let gh_distor = *distro_hostnames
        .get("maven.pkg.github.com")
        .unwrap()
        .value();
    let gh_external = *external_repo_hostnames
        .get("maven.pkg.github.com")
        .unwrap()
        .value();

    let popular_distros = biggest_n(distro_hostnames, 15);
    let popular_repos = biggest_n(external_repo_hostnames, 15);

    println!("For a total of {} repos", report.total);

    println!("Most popular distribution repositories: {popular_distros:#?}");
    println!("Most popular external repositoreis: {popular_repos:#?}");

    println!("Github distro: {}", gh_distor);
    println!("Github external: {}", gh_external);

    Ok(())
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

        let res: Vec<_> = projects
            .par_iter()
            .filter_map(|dir| match process_folder(dir, build_effective) {
                Ok(project) => Some(project),
                Err(error) => {
                    errors.lock().unwrap().push(format!("{error:?}"));
                    None
                }
            })
            .map(|mut proj| {
                // Remove repo maven from external repos
                proj.repos.remove("https://repo.maven.apache.org/maven2");

                if !proj.repos.is_empty() {
                    has_external_repo.fetch_add(1, Ordering::SeqCst);
                }

                if !proj.dist_repos.is_empty() {
                    has_distro_repo.lock().unwrap().push(proj.name.clone());
                }

                for repo in proj.repos.iter() {
                    repos
                        .entry(repo.clone())
                        .and_modify(|el| *el += 1)
                        .or_insert(1);
                }

                for repo in proj.dist_repos.iter() {
                    distros
                        .entry(repo.clone())
                        .and_modify(|el| *el += 1)
                        .or_insert(1);
                }

                let total = total.fetch_add(1, Ordering::SeqCst) + 1;
                if total > 0 && total % 1024 == 0 {
                    info!("Progress: {total}, writing report");
                    if let Err(err) = data.write_report(Report {
                        distros: distros.clone(),
                        external_repos: repos.clone(),
                        has_external_repos: has_external_repo.load(Ordering::SeqCst),
                        has_distro_repos: has_distro_repo.lock().unwrap().clone(),
                        errors: errors.lock().unwrap().clone(),
                        total,
                    }) {
                        error!("Error writing report occurred {err}")
                    }
                }

                proj
            })
            .collect();

        let report = Report {
            distros,
            external_repos: repos,
            has_external_repos: has_external_repo.load(Ordering::SeqCst),
            has_distro_repos: has_distro_repo.lock().unwrap().clone(),
            errors: errors.lock().unwrap().clone(),
            total: total.load(Ordering::SeqCst),
        };

        data.write_report(report.clone()).unwrap();

        data.write_projects(&res).unwrap();

        send.send(report).unwrap();
    });

    let data = recv.await.unwrap();

    Ok(data)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
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
            pom.set_file_name("effective.xml");
            if !pom.exists() {
                pom.set_file_name("pom.xml");
            }
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
            "-T1", // One thread as we don't want maven to interfere with our own multithreading
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
        info!("Created effective pom for {path:?}");

        Ok(pom)
    } else {
        Err(eyre!("Maven command failed"))
    }
}
