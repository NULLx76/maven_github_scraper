use crate::{Data, Repo};
use reqwest::{header, Client, Method, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_derive::{Deserialize, Serialize};
use std::backtrace::Backtrace;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{backtrace, io};
use thiserror::Error;
use tokio::fs;
use tokio::task::yield_now;
use tokio::time::sleep;
use tracing::{error, info, trace, warn};

static USER_AGENT: &str = "rust-repos (https://github.com/rust-ops/rust-repos)";

#[derive(Debug)]
pub struct Github {
    client: Client,
    tokens: Vec<String>,
    current_token_index: AtomicUsize,
    data_dir: Data,
}

#[derive(Deserialize)]
pub struct GitHubError {
    message: String,
    #[serde(rename = "type")]
    type_: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Node {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct GithubTree {
    pub tree: Vec<Node>,
}

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("reqwest error occurred {0:?}")]
    Reqwest(#[from] reqwest::Error),
    #[error("rate limit hit")]
    RateLimit(StatusCode),
    #[error("other http error: {0}")]
    HttpError(StatusCode),

    #[error("Could not get parent pom path")]
    NoParent,
    #[error("IO Error {0}")]
    Io(#[from] io::Error),
}

impl Github {
    pub fn new(tokens: Vec<String>, data: Data) -> Self {
        Github {
            client: Client::new(),
            tokens,
            current_token_index: AtomicUsize::new(0),
            data_dir: data,
        }
    }

    #[inline]
    fn get_token(&self) -> &str {
        &self.tokens[self.current_token_index.load(Ordering::Relaxed)]
    }

    async fn build_request(&self, method: Method, url: String) -> RequestBuilder {
        let url = if !url.starts_with("https://") {
            format!("https://api.github.com/{}", url)
        } else {
            url
        };
        self.client
            .request(method, url)
            .header(header::AUTHORIZATION, format!("token {}", self.get_token()))
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::ACCEPT, "application/vnd.github+json")
    }

    pub async fn tree(&self, repo: &Repo) -> Result<GithubTree, GithubError> {
        let resp = self
            .build_request(
                Method::GET,
                format!("repos/{}/git/trees/HEAD?recursive=1", repo.name),
            )
            .await
            .send()
            .await?;
        handle_response_json(resp).await
    }

    pub async fn download_pom(&self, repo: &Repo, path: &str) -> Result<(), GithubError> {
        if repo.has_pom && path == "pom.xml" {
            return Ok(());
        }

        let url = format!(
            "https://raw.githubusercontent.com/{}/HEAD/{}",
            repo.name, path
        );

        let resp = self.build_request(Method::GET, url).await.send().await?;
        let pom = handle_response(resp).await?.bytes().await?;

        let folder_name = repo.name.replace('/', ".");
        let file = self.data_dir.pom_dir.join(folder_name).join(path);

        fs::create_dir_all(file.parent().ok_or_else(|| GithubError::NoParent)?).await?;

        let mut f = File::create(file)?;
        f.write_all(&pom)?;

        Ok(())
    }

    pub async fn retry<F, Fu, R>(&self, fun: F) -> Result<R, GithubError>
    where
        F: Fn() -> Fu,
        Fu: Future<Output = Result<R, GithubError>>,
    {
        loop {
            match fun().await {
                ok @ Ok(_) => return ok,
                err @ Err(GithubError::Reqwest(_)) => return err,
                Err(err @ GithubError::HttpError(_)) => return Err(err),
                Err(GithubError::RateLimit(_)) => {
                    let mut wait = false;
                    self.current_token_index
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                            if old + 1 >= self.tokens.len() {
                                wait = true;
                                Some(0)
                            } else {
                                Some(old + 1)
                            }
                        })
                        .unwrap();

                    if wait {
                        warn!("Tokens wrapped around, sleeping for 1 minute");
                        sleep(Duration::from_secs(60)).await;
                    }
                }
                err @ Err(_) => return err,
            }
            yield_now().await
        }
    }
}

pub async fn handle_response_json<T: DeserializeOwned>(resp: Response) -> Result<T, GithubError> {
    let res = handle_response(resp).await?.json().await?;
    Ok(res)
}

async fn handle_response(resp: Response) -> Result<Response, GithubError> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::UNPROCESSABLE_ENTITY
    {
        warn!("Rate limit hit");
        Err(GithubError::RateLimit(status))
    } else {
        let error: GitHubError = resp.json().await?;
        if error.message.contains("abuse") || error.message.contains("rate limit") {
            warn!("Rate limit hit ({}): {}", status.as_u16(), error.message);
            Err(GithubError::RateLimit(status))
        } else {
            warn!("Http Error ({}): {}", status.as_u16(), error.message);
            Err(GithubError::HttpError(status))
        }
    }
}
