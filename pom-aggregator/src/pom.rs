use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct Pom {
    pub repositories: Option<Repositories>,
    #[serde(rename = "distributionManagement")]
    pub distribution_management: Option<Repositories>,
}

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct Repositories {
    #[serde(rename = "repository", default)]
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Repository {
    pub id: String,
    pub url: String,
}

impl Pom {
    pub fn repositories(&self) -> Option<Vec<&str>> {
        self.repositories.as_ref().map(|repos| {
            repos
                .repositories
                .iter()
                .map(|repo| repo.url.as_str())
                .collect()
        })
    }

    pub fn distribution_repositories(&self) -> Option<Vec<&str>> {
        self.distribution_management.as_ref().map(|repos| {
            repos
                .repositories
                .iter()
                .map(|repo| repo.url.as_str())
                .collect()
        })
    }
}
