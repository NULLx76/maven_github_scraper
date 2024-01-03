use crate::data::Data;
use crate::scraper::github::{Github, GraphRepository};
use crate::{data, Repo};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{debug, info, warn};

pub mod github;

#[derive(Debug, Clone)]
pub struct Scraper {
    gh: Arc<Github>,
    data: Data,
    finished: Arc<AtomicBool>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Github API Error")]
    Github(#[from] github::Error),
    #[error("Data store error")]
    Data(#[from] data::Error),
}

impl Scraper {
    pub fn new(gh_tokens: Vec<String>, data: Data) -> Self {
        let gh = Github::new(gh_tokens, data.clone());

        Self {
            gh: Arc::new(gh),
            data,
            finished: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn fetch_all_files_for(&self, repo: Repo, file: String) -> Result<bool, Error> {
        let tree = self.gh.tree(&repo).await?;
        let mut js = JoinSet::new();

        let mut has_file = false;

        for f in tree
            .tree
            .into_iter()
            .filter(|node| node.path.ends_with(&file))
        {
            has_file = true;
            let gh = self.gh.clone();
            let repo = repo.clone();

            js.spawn(async move { gh.download_file(&repo, &f.path).await });
        }

        while let Some(res) = js.join_next().await {
            res.unwrap()?;
        }

        info!("Fetched files for {}", &repo.name);

        Ok(has_file)
    }

    async fn load_repositories(&self, repos: Vec<String>) -> Result<(), Error> {
        debug!("Loading {} repos", repos.len());

        let mut graph_repos = self.gh.load_repositories(&repos).await?;
        for repo in graph_repos.drain(..) {
            if repo
                .languages
                .nodes
                .iter()
                .filter_map(Option::as_ref)
                .any(|el| el.name == "Java")
            {
                self.fetch_all_files_for(repo.to_repo(), String::from("pom.xml"))
                    .await?;
            }
        }

        Ok(())
    }

    pub async fn fetch_and_download(&self) -> Result<(), Error> {
        let start = Instant::now();

        let mut to_load = Vec::with_capacity(100);

        let mut last_id = self.data.get_last_id()?;
        loop {
            let start_loop = Instant::now();
            // TODO: Check timeout
            let mut repos = self.gh.scrape_repositories(last_id).await?;
            let finished = self.finished.load(SeqCst);
            let mut js = JoinSet::new();

            for repo in repos.drain(..) {
                last_id = repo.id;
                if repo.fork {
                    continue;
                }

                to_load.push(repo.node_id);

                if to_load.len() == 100 {
                    let to_load_now = to_load.clone();
                    let me = self.clone();
                    js.spawn(async move { me.load_repositories(to_load_now).await });
                    to_load.clear();
                };
            }

            while let Some(res) = js.join_next().await {
                let res = res.unwrap();
                if let Err(e) = res {
                    warn!("Failed scraping repo: {:?}", e);
                }
            }

            if finished {
                if !to_load.is_empty() {
                    let to_load_now = to_load.clone();
                    self.load_repositories(to_load_now).await?;
                }
                break;
            }

            if let Some(time) = Duration::from_secs(1).checked_sub(start_loop.elapsed()) {
                sleep(time).await;
            }
        }

        Ok(())
    }
}
