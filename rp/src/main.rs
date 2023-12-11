use serde::{Deserialize, Serialize};

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

enum Commands {
    /// Get all the java repos from github into a csv file
    FetchJavaRepos,
    /// Per repository, download the poms (recursively)
    DownloadPoms,
    /// Combination of fetch + download
    FetchAndDownload,
    /// Analyze the (effective) poms for the repositories
    Analyze,
}

fn main() {
    println!("Hello, world!");
}
