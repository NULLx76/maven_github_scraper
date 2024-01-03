use crate::data;
use crate::data::Data;
use crate::scraper::github::Github;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

mod github;

#[derive(Debug, Clone)]
struct Scraper {
    gh: Arc<Github>,
    data: Data,
}

#[derive(Debug, Error)]
enum Error {
    #[error("Github API Error")]
    Github(#[from] github::Error),
    #[error("Data store error")]
    Data(#[from] data::Error),
}

impl Scraper {
    pub fn new(gh: Github, data: Data) -> Self {
        Self {
            gh: Arc::new(gh),
            data,
        }
    }

    pub async fn scrape(&self) -> Result<(), Error> {
        let start = Instant::now();

        let last_id = self.data.get_last_id()?;
        loop {
            // TODO: Check timeout
            let mut repos = self.gh.scrape_repositories(last_id).await?;
        }

        Ok(())
    }
}
