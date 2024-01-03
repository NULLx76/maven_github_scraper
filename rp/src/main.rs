use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

mod analyzer;
mod data;
mod scraper;

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub has_pom: bool,
}

impl Repo {
    pub fn path(&self) -> String {
        self.name.replace('/', ".")
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Get all the java repos from github into a csv file
    FetchJavaRepos,
    /// Per repository, download the poms (recursively)
    DownloadPoms,
    /// Combination of fetch + download
    FetchAndDownload,
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
    #[arg(env, hide_env_values = true)]
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

    Ok(())
}
