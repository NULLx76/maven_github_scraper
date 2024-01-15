use crate::data::Data;
use crate::scraper::Scraper;
use clap::{Parser, Subcommand};
use color_eyre::eyre::bail;
use rand::prelude::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, os};

pub mod analyzer;
mod data;
pub mod scraper;

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CsvRepo {
    // Can't use serde(flatten) due to https://github.com/BurntSushi/rust-csv/issues/188
    pub id: String,
    pub name: String,
    pub has_pom: bool,
}

impl From<CsvRepo> for Repo {
    fn from(value: CsvRepo) -> Self {
        Repo {
            id: value.id,
            name: value.name,
        }
    }
}

impl Repo {
    pub fn path(&self) -> String {
        self.name.replace('/', ".")
    }

    pub fn to_csv_repo(self, has_pom: bool) -> CsvRepo {
        CsvRepo {
            id: self.id,
            name: self.name,
            has_pom,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch all Java repos from Github and fetch all pom files of them (recursively)
    FetchAndDownload,

    /// Per repository, only download the poms (recursively)
    /// This uses an already existing csv file
    DownloadPoms,

    /// Analyze the (effective) poms for the repositories
    Analyze {
        /// Create effective poms (~2s per POM)
        #[arg(long)]
        effective: bool,
    },
    CreateRandomSubset {
        n: usize,
        from: PathBuf,
        out: PathBuf,
    },
    /// Updates the has_pom field in the csv to correspond to the filesystem
    ConsolidateCsv,
}

#[derive(Parser)]
struct Cli {
    /// The data directory to analyze or download into
    #[arg(short, long = "data", default_value = "./data/sample10_000")]
    data_dir: PathBuf,

    /// Github tokens to use when fetching from GitHub
    #[arg(env = "GH_TOKENS", hide_env_values = true)]
    tokens: Vec<String>,

    #[command(subcommand)]
    cmd: Commands,
}

const SEED: [u8; 32] = [42; 32];

pub fn create_subset(n: usize, from: PathBuf, out: PathBuf) -> color_eyre::Result<()> {
    let mut rng = ChaCha20Rng::from_seed(SEED);

    let mut reader = csv::Reader::from_path(from.join("github.csv")).unwrap();

    let mut repos: Vec<Repo> = reader.deserialize().map(|el| el.unwrap()).collect();

    repos.shuffle(&mut rng);

    repos.truncate(n);

    fs::create_dir_all(out.join("poms"))?;

    let fetched = from.join("fetched");

    if fetched.exists() {
        fs::copy(fetched, out.join("fetched"))?;
    }

    let mut writer = csv::Writer::from_path(out.join("github.csv")).unwrap();
    for repo in repos {
        let repo_path = repo.name.replace('/', ".");
        if let Ok(path) = from.join("poms").join(&repo_path).canonicalize() {
            if path.exists() {
                symlink(path, out.join("poms").join(&repo_path))?;
            }
        }

        writer.serialize(&repo).unwrap();
    }

    Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    dotenv::dotenv().ok();
    color_eyre::install().unwrap();

    console_subscriber::ConsoleLayer::builder()
        .retention(Duration::from_secs(60))
        .init();

    let cli = Cli::parse();

    if cli.tokens.is_empty() {
        bail!("Please provide Github Tokens");
    }

    let data = Data::new(cli.data_dir.as_path()).await?;

    match cli.cmd {
        Commands::FetchAndDownload => {
            let scraper = Scraper::new(cli.tokens, data.clone());
            scraper.fetch_and_download().await?;
        }
        Commands::DownloadPoms => {
            let scraper = Scraper::new(cli.tokens, data.clone());
            scraper.download_files().await?;
            data.update_csv_has_pom().await?;
        }
        Commands::Analyze { effective } => {
            let report = analyzer::analyze(data, effective).await?;
            report.print();
        }
        Commands::CreateRandomSubset { n, from, out } => {
            create_subset(n, from, out)?;
        }
        Commands::ConsolidateCsv => {
            data.update_csv_has_pom().await?;
        }
    }

    Ok(())
}
