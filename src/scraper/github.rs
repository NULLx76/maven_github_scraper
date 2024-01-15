use crate::data::Data;
use crate::{data, Repo};
use reqwest::{header, Client, Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::task::yield_now;
use tokio::time::sleep;
use tracing::{error, warn};

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

#[derive(Debug, Deserialize)]
pub struct RestRepository {
    pub id: usize,
    pub full_name: String,
    pub node_id: String,
    pub fork: bool,
}

#[derive(Deserialize)]
struct GraphResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GitHubError>>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct GraphRateLimit {
    cost: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphRepositories {
    nodes: Vec<Option<GraphRepository>>,
    rate_limit: GraphRateLimit,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRepository {
    pub id: String,
    pub name_with_owner: String,
    pub languages: GraphLanguages,
}

impl GraphRepository {
    pub fn to_repo(self) -> Repo {
        Repo {
            id: self.id,
            name: self.name_with_owner,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GraphLanguages {
    pub nodes: Vec<Option<GraphLanguage>>,
}

#[derive(Debug, Deserialize)]
pub struct GraphLanguage {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct GraphRef {
    pub name: String,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("reqwest error occurred {0:?}")]
    Reqwest(#[from] reqwest::Error),
    #[error("rate limit hit {0}")]
    RateLimit(StatusCode),
    #[error("other http error: {0}")]
    HttpError(StatusCode),

    #[error("Data error occurred: {0:?}")]
    DataError(#[from] data::Error),

    #[error("Response did not contain requested data")]
    EmptyData,
    #[error("IO Error {0}")]
    Io(#[from] io::Error),
}

const GRAPHQL_QUERY_REPOSITORIES: &str = "
query($ids: [ID!]!) {
    nodes(ids: $ids) {
        ... on Repository {
            id
            nameWithOwner
            languages(first: 100, orderBy: { field: SIZE, direction: DESC }) {
                nodes {
                    name
                }
            }
        }
    }

    rateLimit {
        cost
    }
}
";

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

    async fn build_request(&self, method: Method, url: &str) -> RequestBuilder {
        let url = if !url.starts_with("https://") {
            Cow::from(format!("https://api.github.com/{}", url))
        } else {
            Cow::from(url)
        };
        self.client
            .request(method, url.as_ref())
            .header(header::AUTHORIZATION, format!("token {}", self.get_token()))
            .header(header::USER_AGENT, USER_AGENT)
        // .header(header::ACCEPT, "application/vnd.github+json")
    }

    async fn graphql<T: DeserializeOwned, V: Serialize>(
        &self,
        query: &str,
        variables: V,
    ) -> Result<T, Error> {
        let resp = self
            .build_request(Method::POST, "graphql")
            .await
            .json(&json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await?;

        let data: GraphResponse<T> = handle_response_json(resp).await?;

        data.data.ok_or_else(|| Error::EmptyData)
    }

    pub async fn load_repositories(
        &self,
        node_ids: &[String],
    ) -> Result<Vec<GraphRepository>, Error> {
        let data: GraphRepositories = self
            .retry(|| async {
                self.graphql(
                    GRAPHQL_QUERY_REPOSITORIES,
                    json!({
                        "ids": node_ids,
                    }),
                )
                .await
            })
            .await?;

        assert!(
            data.rate_limit.cost <= 1,
            "load repositories query too costly"
        );

        Ok(data.nodes.into_iter().flatten().collect())
    }

    /// gets a file tree of a specific github repo
    pub async fn tree(&self, repo: &Repo) -> Result<GithubTree, Error> {
        self.retry(|| async {
            let resp = self
                .build_request(
                    Method::GET,
                    &format!("repos/{}/git/trees/HEAD?recursive=1", repo.name),
                )
                .await
                .send()
                .await?;

            handle_response_json(resp).await
        })
        .await
    }

    /// scrapes all github repos (paginated)
    pub async fn scrape_repositories(&self, since: usize) -> Result<Vec<RestRepository>, Error> {
        // Maybe needs to be a Vec<Option<RestRepository>>
        let output: Vec<RestRepository> = self
            .retry(|| async {
                let resp = self
                    .build_request(Method::GET, &format!("repositories?since={}", since))
                    .await
                    .send()
                    .await?;

                handle_response_json(resp).await
            })
            .await?;

        Ok(output)
    }

    /// downloads a file from a github repo
    ///
    /// path being the path inside the repo
    pub async fn download_file(&self, repo: &Repo, path: &str) -> Result<(), Error> {
        let file = self.data_dir.get_pom_path(repo, path);
        if file.exists() {
            return Ok(());
        }

        let url = format!(
            "https://raw.githubusercontent.com/{}/HEAD/{}",
            repo.name, path
        );

        let bytes = self
            .retry(|| async {
                let resp = self.build_request(Method::GET, &url).await.send().await?;
                let pom = handle_response(resp).await?.bytes().await?;
                Ok(pom)
            })
            .await?;

        self.data_dir.write_pom(repo, path, &bytes).await?;

        Ok(())
    }

    pub async fn has_github_releases(&self, repo: &Repo) -> Result<bool, Error> {
        let releases: Vec<Value> = self
            .retry(|| async {
                let resp = self
                    .build_request(Method::GET, &format!("repos/{}/releases", repo.name))
                    .await
                    .send()
                    .await?;
                let resp = handle_response_json(resp).await?;

                Ok(resp)
            })
            .await?;

        Ok(!releases.is_empty())
    }

    /// retry a github api request and rotate tokens to circumvent rate limiting
    async fn retry<F, Fu, R>(&self, fun: F) -> Result<R, Error>
    where
        F: Fn() -> Fu,
        Fu: Future<Output = Result<R, Error>>,
    {
        loop {
            match fun().await {
                ok @ Ok(_) => return ok,
                err @ Err(Error::Reqwest(_)) => return err,
                Err(err @ Error::HttpError(_)) => return Err(err),
                Err(Error::RateLimit(_)) => {
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

async fn handle_response_json<T: DeserializeOwned>(resp: Response) -> Result<T, Error> {
    let res = handle_response(resp).await?.json().await?;
    Ok(res)
}

/// Converts github responses into the correct error codes (helper for the retry function)
async fn handle_response(resp: Response) -> Result<Response, Error> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::UNPROCESSABLE_ENTITY
    {
        warn!("Rate limit hit");
        Err(Error::RateLimit(status))
    } else if let Ok(error) = resp.json().await {
        let error: GitHubError = error;
        if error.message.contains("abuse") || error.message.contains("rate limit") {
            warn!("Rate limit hit ({}): {}", status.as_u16(), error.message);
            Err(Error::RateLimit(status))
        } else {
            warn!("Http Error ({}): {}", status.as_u16(), error.message);
            Err(Error::HttpError(status))
        }
    } else {
        Err(Error::HttpError(status))
    }
}
