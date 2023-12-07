use crate::api::{Github, GithubError, GithubTree};
use crate::Repo;
use std::sync::Arc;
use thiserror::Error;
use tokio::join;
use tokio::task::JoinSet;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("Github API Error")]
    Request(#[from] GithubError),
}

pub async fn fetch_all_poms_for(gh: Arc<Github>, repo: &Repo) -> Result<(), FetchError> {
    let tree = match gh.retry(|| async { gh.tree(&repo).await }).await {
        Ok(v) => v,
        Err(GithubError::HttpError(_)) => return Ok(()), // Silently ignore
        err @ Err(_) => err?,
    };
    let mut js = JoinSet::new();

    let repo = Arc::new(repo.clone());
    for pom in tree
        .tree
        .into_iter()
        .filter(|node| node.path.ends_with("pom.xml"))
    {
        let gh = gh.clone();
        let repo = repo.clone();
        // https://github.com/tokio-rs/tokio/pull/6158
        js.spawn(async move {
            gh.retry(|| async { gh.download_pom(&*repo, &pom.path).await })
                .await
        });
    }

    while let Some(res) = js.join_next().await {
        match res.unwrap() {
            Err(err @ GithubError::HttpError(_)) => {
                warn!("HTTP Error: {err}");
            }
            err @ Err(_) => err?,
            Ok(_) => {}
        }
    }

    info!("Fetched poms for {}", &repo.name);

    Ok(())
}
