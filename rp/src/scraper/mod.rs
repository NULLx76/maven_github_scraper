use crate::data::Data;
use crate::scraper::github::Github;
use std::sync::Arc;
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
}

impl Scraper {
    pub fn new(gh: Github, data: Data) -> Self {
        Self {
            gh: Arc::new(gh),
            data,
        }
    }

    pub fn scrape(&self) -> Result<(), Error> {}
}
