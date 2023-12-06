use crate::api::Github;
use crate::fetcher::fetch_all_poms_for;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tracing::info;

mod api;
mod fetcher;

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub has_pom: bool,
}

#[derive(Debug, Clone)]
pub struct Data {
    pub data_dir: PathBuf,
    pub pom_dir: PathBuf,
    pub githuv_csv: PathBuf,
    pub fetched: PathBuf,
}

impl Data {
    pub fn new(data_dir: impl AsRef<Path>) -> Result<Self, io::Error> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let data = Data {
            data_dir: data_dir.clone(),
            pom_dir: data_dir.join("poms"),
            githuv_csv: data_dir.join("github.csv"),
            fetched: data_dir.join("fetched"),
        };
        if !data_dir.exists() {
            std::fs::File::create(&data_dir)?;
        }

        Ok(data)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv::dotenv().ok();
    console_subscriber::init();
    info!("Starting");

    let tokens = env::var("GH_TOKENS")?
        .split(',')
        .map(str::to_string)
        .collect();

    let data = Data::new("./data/poms")?;

    let gh = Arc::new(Github::new(tokens, data.clone()));

    iterate(gh, data).await.unwrap();

    Ok(())
}

#[derive(Debug, Error)]
pub enum IterateError {
    #[error("CSV Reading Error")]
    Csv(#[from] csv::Error),

    #[error("Error Writing Log file")]
    Io(#[from] io::Error),
}

pub async fn iterate(gh: Arc<Github>, data: Data) -> Result<(), IterateError> {
    let mut reader = csv::Reader::from_path("./data/github.csv").unwrap();
    let mut js: JoinSet<Result<(), io::Error>> = JoinSet::new();

    let data = Arc::new(data);

    if !data.fetched.exists() {
        File::create(&data.fetched).await?;
    }
    let mut f = File::open(&data.fetched).await?;

    let mut done = String::new();
    f.read_to_string(&mut done).await?;

    let done: HashSet<_> = done.lines().collect();

    info!("Skipping {} repos", done.len());

    for entry in reader.deserialize().filter(|el| {
        el.as_ref()
            .is_ok_and(|el: &Repo| !done.contains(&el.id.as_str()))
    }) {
        let repo: Repo = entry.unwrap();
        fetch_all_poms_for(gh.clone(), &repo).await.unwrap();
        let data = data.clone();
        js.spawn(async move {
            let mut file = OpenOptions::new()
                .write(true)
                .append(true)
                .open(&data.fetched)
                .await?;
            file.write_all(repo.id.as_bytes()).await?;
            file.write_all("\n".as_bytes()).await?;

            Ok(())
        });
    }

    while let Some(res) = js.join_next().await {
        res.unwrap()?;
    }

    Ok(())
}
