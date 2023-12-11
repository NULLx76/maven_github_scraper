use serde::{Deserialize, Serialize};

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

fn main() {
    println!("Hello, world!");
}
