use crate::data::Data;
use crate::scraper::Scraper;
use clap::{Parser, Subcommand};
use color_eyre::eyre::bail;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

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
        effective: bool,
    },
}

#[derive(Parser)]
struct Cli {
    /// The data directory to analyze or download into
    #[arg(short, long = "data", default_value = "../data/sample10_000")]
    data_dir: PathBuf,

    /// Github tokens to use when fetching from GitHub
    #[arg(env = "GH_TOKENS", hide_env_values = true)]
    tokens: Vec<String>,

    #[command(subcommand)]
    cmd: Commands,
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
        }
        Commands::Analyze { .. } => todo!(),
    }

    Ok(())
}
