use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "listen")]
    pub listen: SocketAddr,
    pub token_env: Option<String>,
    pub roots: Vec<RootConfig>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub name: String,
    pub path: PathBuf,
}
fn listen() -> SocketAddr {
    "127.0.0.1:3210".parse().unwrap()
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.listen.ip().is_loopback(), "LISTEN_NOT_LOOPBACK");
        ensure!(!self.roots.is_empty(), "ROOTS_EMPTY");
        let mut names = std::collections::HashSet::new();
        for root in &self.roots {
            ensure!(
                !root.name.is_empty()
                    && root.name.len() <= 64
                    && root
                        .name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "INVALID_ROOT_NAME"
            );
            ensure!(names.insert(&root.name), "DUPLICATE_ROOT_NAME");
            ensure!(root.path.is_absolute(), "ROOT_MUST_BE_ABSOLUTE");
        }
        Ok(())
    }
}
