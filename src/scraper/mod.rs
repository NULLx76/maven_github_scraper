use crate::data::Data;
use crate::scraper::github::Github;
use crate::{data, Repo};
use itertools::Itertools;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::signal::ctrl_c;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

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
        let finished = Arc::new(AtomicBool::new(false));
        let f2 = finished.clone();

        tokio::spawn(async move {
            ctrl_c().await.expect("Failed to install Ctrl+C Handler");
            warn!("Ctrl+C received, stopping...");
            f2.store(true, SeqCst);
        });

        Self {
            gh: Arc::new(gh),
            data,
            finished,
        }
    }

    async fn has_github_releases(&self, repo: &Repo) -> Result<bool, Error> {
        let res = self.gh.has_github_releases(repo).await?;
        todo!("write to file somewhere")
    }

    pub async fn download_all_workflows(&self) -> Result<usize, Error> {
        let report = self.data.read_report()?;
        let mut cnt = 0;
        for repos in report
            .has_distro_repos
            .into_iter()
            .tuples::<(_, _, _, _, _)>()
        {
            let mut js = JoinSet::new();
            let repos: [String; 5] = repos.into();
            for repo in repos {
                let repo = Repo {
                    id: String::default(),
                    name: repo.replace('.', "/"),
                };

                let me = self.clone();
                js.spawn(async move { me.fetch_workflow_files(&repo).await });
            }

            while let Some(next) = js.join_next().await {
                match next.unwrap() {
                    Ok(true) => cnt += 1,
                    Err(e) => error!("Error: {e:?}"),
                    _ => {}
                }
            }
        }

        Ok(cnt)
    }

    async fn fetch_workflow_files(&self, repo: &Repo) -> Result<bool, Error> {
        let tree = self.gh.tree(repo).await?;
        let mut js = JoinSet::new();

        let mut has_file = false;

        for f in tree.tree.into_iter().filter(|node| {
            node.path.starts_with(".github/workflows")
                && (node.path.ends_with(".yml") || node.path.ends_with(".yaml"))
        }) {
            has_file = true;
            let gh = self.gh.clone();
            let repo = repo.clone();

            info!("Downloading {:?}, {}", &repo, &f.path);
            js.spawn(async move { gh.download_file(&repo, &f.path).await });
        }

        while let Some(res) = js.join_next().await {
            res.unwrap()?;
        }

        self.data.mark_fetched(repo).await?;
        info!("Fetched files for {}", &repo.name);

        Ok(has_file)
    }

    async fn fetch_all_files_for(&self, repo: &Repo, file: String) -> Result<bool, Error> {
        debug!("Fetching files for {}", repo.name);
        let tree = match self.gh.tree(repo).await {
            Ok(el) => el,
            Err(github::Error::HttpError(code)) => {
                self.data.mark_fetched(repo).await?;
                warn!(
                    "HTTP Error occurred {code} while getting tree for {}",
                    repo.name
                );
                return Ok(false);
            }
            e @ Err(_) => e?,
        };
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
            if let Err(e) = res.unwrap() {
                match e {
                    github::Error::HttpError(code) => {
                        warn!(
                            "HTTP {} occurred while fetching files for {}",
                            code.as_u16(),
                            repo.name
                        )
                    }
                    e => return Err(e.into()),
                }
            }
        }

        self.data.mark_fetched(repo).await?;
        info!("Fetched files for {}", &repo.name);

        Ok(has_file)
    }

    async fn load_repositories(&self, repos: Vec<String>) -> Result<(), Error> {
        info!("Loading {} repos", repos.len());

        let mut graph_repos = self.gh.load_repositories(&repos).await?;
        for repo in graph_repos.drain(..) {
            if repo
                .languages
                .nodes
                .iter()
                .filter_map(Option::as_ref)
                .any(|el| el.name == "Java")
            {
                let repo = repo.to_repo();
                let has_files = self
                    .fetch_all_files_for(&repo, String::from("pom.xml"))
                    .await?;

                self.data.store_repo(repo.to_csv_repo(has_files)).await?;
            }
        }

        Ok(())
    }

    pub async fn download_files(&self) -> Result<(), Error> {
        let repos = self.data.get_non_fetched_repos().await?;

        for repo in repos {
            if self.finished.load(SeqCst) {
                break;
            }
            self.fetch_all_files_for(&repo.into(), String::from("pom.xml"))
                .await?;
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

            self.data.set_last_id(last_id).await.unwrap();

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

            if let Some(time) = Duration::from_millis(250).checked_sub(start_loop.elapsed()) {
                sleep(time).await;
            }
        }

        info!("Took {} seconds", start.elapsed().as_secs());

        Ok(())
    }
}
