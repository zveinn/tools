use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const APP_DIR: &str = ".gitsync";
const LEGACY_DIR: &str = ".github-feed";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub username: Option<String>,
    pub token: Option<String>,
    pub api_url: String,
    pub allowed_repos: Vec<String>,
    pub poll_seconds: u64,
    pub backfill_days: u32,
    pub participating_only: bool,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            username: None,
            token: None,
            api_url: "https://api.github.com".into(),
            allowed_repos: Vec::new(),
            poll_seconds: 90,
            backfill_days: 90,
            participating_only: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub username: Option<String>,
    pub token: Option<String>,
    pub api_url: String,
    pub graphql_url: String,
    pub allowed_repos: Vec<String>,
    pub poll_seconds: u64,
    pub backfill_days: u32,
    pub participating_only: bool,
    pub dir: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub offline: bool,
}

impl Config {
    pub fn load(offline: bool) -> Result<Self> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let dir = home.join(APP_DIR);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        let cfg_path = dir.join("config.toml");
        if !cfg_path.exists() {
            let template = default_config_toml();
            fs::write(&cfg_path, template)
                .with_context(|| format!("write {}", cfg_path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o600));
            }
        }

        let file: FileConfig = {
            let raw = fs::read_to_string(&cfg_path)
                .with_context(|| format!("read {}", cfg_path.display()))?;
            toml::from_str(&raw).with_context(|| format!("parse {}", cfg_path.display()))?
        };

        let legacy_env = home.join(LEGACY_DIR).join(".env");
        let legacy = load_dotenv(&legacy_env).unwrap_or_default();

        let token = first_nonempty(&[
            std::env::var("GITHUB_TOKEN").ok(),
            std::env::var("GITHUB_ACTIVITY_TOKEN").ok(),
            std::env::var("GH_TOKEN").ok(),
            read_optional(dir.join("token")),
            file.token.clone(),
            legacy.get("GITHUB_TOKEN"),
            legacy.get("GITHUB_ACTIVITY_TOKEN"),
        ]);

        let username = first_nonempty(&[
            std::env::var("GITHUB_USERNAME").ok(),
            std::env::var("GITHUB_USER").ok(),
            file.username.clone(),
            legacy.get("GITHUB_USERNAME"),
            legacy.get("GITHUB_USER"),
        ]);

        let allowed = if let Ok(raw) = std::env::var("ALLOWED_REPOS") {
            split_repos(&raw)
        } else if !file.allowed_repos.is_empty() {
            file.allowed_repos.clone()
        } else {
            legacy
                .get("ALLOWED_REPOS")
                .map(|s| split_repos(&s))
                .unwrap_or_default()
        };

        let api_url = std::env::var("GITHUB_API_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or(file.api_url);
        let api_url = api_url.trim_end_matches('/').to_string();
        let graphql_url = graphql_from_api(&api_url);

        if let Some(ref t) = token {
            validate_token(t)?;
        } else if !offline {
            // Allowed: TUI can still open the cache. Sync will explain what's missing.
        }

        Ok(Self {
            username,
            token,
            api_url,
            graphql_url,
            allowed_repos: allowed,
            poll_seconds: file.poll_seconds.max(30),
            backfill_days: file.backfill_days.max(1),
            participating_only: file.participating_only,
            db_path: dir.join("gitsync.db"),
            log_path: dir.join("gitsync.log"),
            dir,
            offline,
        })
    }

    pub fn has_token(&self) -> bool {
        self.token.as_deref().is_some_and(|t| !t.is_empty())
    }
}

fn graphql_from_api(api_url: &str) -> String {
    if api_url == "https://api.github.com" || api_url == "http://api.github.com" {
        format!("{api_url}/graphql")
    } else if let Some(base) = api_url.strip_suffix("/api/v3") {
        format!("{base}/api/graphql")
    } else {
        format!("{api_url}/graphql")
    }
}

fn validate_token(token: &str) -> Result<()> {
    let ok = token.starts_with("ghp_")
        || token.starts_with("gho_")
        || token.starts_with("ghu_")
        || token.starts_with("ghs_")
        || token.starts_with("github_pat_");
    if !ok {
        bail!(
            "GitHub token looks invalid (expected ghp_/gho_/github_pat_ prefix). Check ~/.gitsync/config.toml or GITHUB_TOKEN."
        );
    }
    Ok(())
}

fn first_nonempty(opts: &[Option<String>]) -> Option<String> {
    opts.iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

fn split_repos(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn read_optional(path: PathBuf) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[derive(Default)]
struct DotEnv(std::collections::HashMap<String, String>);

impl DotEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

fn load_dotenv(path: &Path) -> Result<DotEnv> {
    let file = fs::File::open(path)?;
    let mut map = std::collections::HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    Ok(DotEnv(map))
}

fn default_config_toml() -> &'static str {
    r#"# xgit configuration
# Token is optional here — GITHUB_TOKEN / GH_TOKEN / ~/.gitsync/token /
# ~/.github-feed/.env are also checked.

# username is discovered from the token if left empty
# username = "your-login"

# token = "ghp_..."

api_url = "https://api.github.com"

# Leave empty to include every repo you are involved in
allowed_repos = []

# How often the TUI asks GitHub "anything new?" (notifications + 304 shortcut)
poll_seconds = 90

# How far back the first (and any forced) backfill search goes
backfill_days = 90

# Only notifications you participate in (not every watched repo)
participating_only = false
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_urls() {
        assert_eq!(
            graphql_from_api("https://api.github.com"),
            "https://api.github.com/graphql"
        );
        assert_eq!(
            graphql_from_api("https://git.example.com/api/v3"),
            "https://git.example.com/api/graphql"
        );
    }

    #[test]
    fn token_prefix() {
        assert!(validate_token("ghp_abc").is_ok());
        assert!(validate_token("github_pat_abc").is_ok());
        assert!(validate_token("not-a-token").is_err());
    }
}
