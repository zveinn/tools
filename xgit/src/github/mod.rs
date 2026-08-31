mod parse;
mod queries;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::config::Config;
use crate::model::{HydratedItem, Role};

pub use parse::comments_from_value;

const USER_AGENT: &str = concat!("xgit/", env!("CARGO_PKG_VERSION"));
const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, Default)]
pub struct RateSnapshot {
    pub graphql_remaining: Option<u32>,
    pub graphql_limit: Option<u32>,
    pub graphql_reset: Option<String>,
    pub last_cost: Option<u32>,
    pub poll_interval: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: String,
    pub unread: bool,
    pub reason: String,
    pub updated_at: String,
    pub subject_type: String,
    pub title: String,
    pub owner: String,
    pub repo: String,
    pub number: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ItemRef {
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub node_id: Option<String>,
}

#[derive(Debug)]
pub struct SearchPage {
    pub items: Vec<HydratedItem>,
    pub cursor: Option<String>,
    pub has_next: bool,
    pub issue_count: i64,
}

pub struct GhClient {
    http: reqwest::Client,
    api_url: String,
    graphql_url: String,
    pub rate: std::sync::Mutex<RateSnapshot>,
}

impl GhClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let token = cfg
            .token
            .clone()
            .context("GitHub token is required for network access")?;

        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static(API_VERSION),
        );
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid token for Authorization header")?;
        auth.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(45))
            .build()?;

        let _ = token;
        Ok(Self {
            http,
            api_url: cfg.api_url.clone(),
            graphql_url: cfg.graphql_url.clone(),
            rate: std::sync::Mutex::new(RateSnapshot::default()),
        })
    }

    pub fn rate_snapshot(&self) -> RateSnapshot {
        self.rate.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub async fn viewer_login(&self) -> Result<String> {
        let data = self
            .graphql(queries::VIEWER, json!({}))
            .await
            .context("viewer login")?;
        let login = data
            .pointer("/viewer/login")
            .and_then(Value::as_str)
            .context("viewer.login missing")?
            .to_string();
        Ok(login)
    }

    pub async fn search(&self, query: &str, after: Option<&str>) -> Result<SearchPage> {
        let data = self
            .graphql(&queries::search(), json!({ "q": query, "after": after }))
            .await
            .with_context(|| format!("search {query}"))?;

        let issue_count = data
            .pointer("/search/issueCount")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let has_next = data
            .pointer("/search/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let cursor = data
            .pointer("/search/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut items = Vec::new();
        if let Some(nodes) = data.pointer("/search/nodes").and_then(Value::as_array) {
            for node in nodes {
                if let Some(item) = parse::item_from_value(node) {
                    items.push(item);
                }
            }
        }
        Ok(SearchPage {
            items,
            cursor,
            has_next,
            issue_count,
        })
    }

    pub async fn hydrate(&self, refs: &[ItemRef]) -> Result<Vec<HydratedItem>> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let with_ids: Vec<&str> = refs.iter().filter_map(|r| r.node_id.as_deref()).collect();
        if with_ids.len() == refs.len() {
            return self.hydrate_nodes(&with_ids).await;
        }
        self.hydrate_aliases(refs).await
    }

    async fn hydrate_nodes(&self, ids: &[&str]) -> Result<Vec<HydratedItem>> {
        let data = self
            .graphql(&queries::nodes(), json!({ "ids": ids }))
            .await
            .context("hydrate nodes")?;
        let mut items = Vec::new();
        if let Some(nodes) = data.get("nodes").and_then(Value::as_array) {
            for node in nodes {
                if node.is_null() {
                    continue;
                }
                if let Some(item) = parse::item_from_value(node) {
                    items.push(item);
                }
            }
        }
        Ok(items)
    }

    async fn hydrate_aliases(&self, refs: &[ItemRef]) -> Result<Vec<HydratedItem>> {
        let mut decls = Vec::new();
        let mut fields = String::new();
        let mut variables = serde_json::Map::new();
        for (i, r) in refs.iter().enumerate() {
            decls.push(format!("$o{i}: String!, $n{i}: String!, $num{i}: Int!"));
            fields.push_str(&format!(
                "i{i}: repository(owner: $o{i}, name: $n{i}) {{ issueOrPullRequest(number: $num{i}) {{ ...Item }} }}\n"
            ));
            variables.insert(format!("o{i}"), json!(r.owner));
            variables.insert(format!("n{i}"), json!(r.repo));
            variables.insert(format!("num{i}"), json!(r.number));
        }
        let query = format!(
            "{}\n{}\nquery Hydrate({}) {{\n{fields}  rateLimit {{ cost remaining resetAt limit }}\n}}",
            queries::ITEM_FRAGMENTS,
            queries::ITEM_ON_UNION,
            decls.join(", "),
        );
        let data = self
            .graphql(&query, Value::Object(variables))
            .await
            .context("hydrate aliases")?;
        let mut items = Vec::new();
        if let Some(obj) = data.as_object() {
            for (k, v) in obj {
                if k == "rateLimit" {
                    continue;
                }
                if let Some(node) = v.get("issueOrPullRequest") {
                    if let Some(item) = parse::item_from_value(node) {
                        items.push(item);
                    }
                }
            }
        }
        Ok(items)
    }

    pub async fn comments(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> Result<Vec<crate::model::Comment>> {
        let data = self
            .graphql(
                queries::COMMENTS,
                json!({ "owner": owner, "name": repo, "number": number }),
            )
            .await
            .context("comments")?;
        let node = data
            .pointer("/repository/issueOrPullRequest")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(parse::comments_from_value(&node))
    }

    /// Incremental change feed. `304 Not Modified` is a success with zero items
    /// and does not consume the REST rate limit.
    pub async fn notifications(
        &self,
        since: Option<&str>,
        etag: Option<&str>,
        participating: bool,
    ) -> Result<NotifResult> {
        let mut url = format!(
            "{}/notifications?all=true&participating={}&per_page=50",
            self.api_url, participating
        );
        if let Some(since) = since {
            url.push_str("&since=");
            url.push_str(&urlencoding_lite(since));
        }

        let mut last_etag = etag.map(str::to_string);
        let mut last_modified: Option<String> = None;
        let mut poll_interval: Option<u64> = None;
        let mut page = 1;
        let mut items = Vec::new();

        loop {
            let page_url = format!("{url}&page={page}");
            let mut req = self.http.get(&page_url);
            if page == 1 {
                if let Some(et) = etag {
                    if let Ok(v) = HeaderValue::from_str(et) {
                        req = req.header(header::IF_NONE_MATCH, v);
                    }
                }
            }

            let resp = self.send_rest(req).await?;
            if let Some(v) = resp.headers().get("X-Poll-Interval") {
                poll_interval = v.to_str().ok().and_then(|s| s.parse().ok());
            }
            if let Some(v) = resp.headers().get(header::ETAG) {
                last_etag = v.to_str().ok().map(str::to_string);
            }
            if let Some(v) = resp.headers().get(header::LAST_MODIFIED) {
                last_modified = v.to_str().ok().map(str::to_string);
            }

            if resp.status() == StatusCode::NOT_MODIFIED {
                self.store_poll(poll_interval);
                return Ok(NotifResult {
                    items: Vec::new(),
                    etag: last_etag,
                    last_modified,
                    poll_interval,
                    not_modified: true,
                });
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!("notifications {status}: {body}");
            }

            let link_next = resp
                .headers()
                .get(header::LINK)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let page_items: Vec<RawNotification> =
                resp.json().await.context("decode notifications")?;
            for raw in page_items {
                if let Some(n) = raw.into_notif() {
                    items.push(n);
                }
            }

            let has_next = link_next
                .as_deref()
                .is_some_and(|l| l.contains("rel=\"next\""));
            if !has_next || page >= 10 {
                break;
            }
            page += 1;
        }

        self.store_poll(poll_interval);
        Ok(NotifResult {
            items,
            etag: last_etag,
            last_modified,
            poll_interval,
            not_modified: false,
        })
    }

    fn store_poll(&self, poll_interval: Option<u64>) {
        if let Ok(mut g) = self.rate.lock() {
            g.poll_interval = poll_interval;
        }
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let mut last_err = None;
        for attempt in 1..=8 {
            let req = self.http.post(&self.graphql_url).json(&body);
            match self.send_rest(req).await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
                        let wait = retry_after(&resp)
                            .unwrap_or(Duration::from_secs(2u64.pow(attempt.min(5))));
                        warn!("GraphQL {status}, waiting {wait:?} (attempt {attempt})");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        last_err = Some(anyhow::anyhow!("GraphQL HTTP {status}: {text}"));
                        tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                        continue;
                    }
                    let parsed: GqlResponse = resp.json().await.context("decode GraphQL JSON")?;
                    if let Some(rl) = parsed
                        .data
                        .as_ref()
                        .and_then(|d| d.get("rateLimit"))
                        .cloned()
                    {
                        self.record_rate(&rl);
                    }
                    if let Some(errors) = parsed.errors {
                        let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
                        let fatal = parsed.data.is_none()
                            || parsed.data.as_ref().is_some_and(|d| d.is_null());
                        if fatal {
                            last_err = Some(anyhow::anyhow!("GraphQL errors: {}", msgs.join("; ")));
                            tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                            continue;
                        }
                        debug!("GraphQL partial errors: {}", msgs.join("; "));
                    }
                    return parsed.data.context("GraphQL response missing data");
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("GraphQL failed")))
    }

    async fn send_rest(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut last_err = None;
        for attempt in 1..=8 {
            match req.try_clone().context("clone request")?.send().await {
                Ok(resp) => {
                    if resp.status() == StatusCode::FORBIDDEN
                        || resp.status() == StatusCode::TOO_MANY_REQUESTS
                    {
                        if let Some(wait) = retry_after(&resp) {
                            warn!("REST {}, waiting {wait:?}", resp.status());
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    last_err = Some(e.into());
                    tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("request failed")))
    }

    fn record_rate(&self, rl: &Value) {
        if let Ok(mut g) = self.rate.lock() {
            g.graphql_remaining = rl
                .get("remaining")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
            g.graphql_limit = rl.get("limit").and_then(Value::as_u64).map(|n| n as u32);
            g.graphql_reset = rl
                .get("resetAt")
                .and_then(Value::as_str)
                .map(str::to_string);
            g.last_cost = rl.get("cost").and_then(Value::as_u64).map(|n| n as u32);
        }
    }
}

#[derive(Debug)]
pub struct NotifResult {
    pub items: Vec<Notification>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub poll_interval: Option<u64>,
    pub not_modified: bool,
}

impl Notification {
    pub fn item_ref(&self) -> Option<ItemRef> {
        Some(ItemRef {
            owner: self.owner.clone(),
            repo: self.repo.clone(),
            number: self.number?,
            node_id: None,
        })
    }

    pub fn extra_role(&self) -> Option<Role> {
        Role::from_notification_reason(&self.reason)
    }

    pub fn is_item(&self) -> bool {
        self.number.is_some() && matches!(self.subject_type.as_str(), "Issue" | "PullRequest")
    }
}

#[derive(Debug, Deserialize)]
struct GqlResponse {
    data: Option<Value>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Debug, Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct RawNotification {
    id: Option<String>,
    unread: Option<bool>,
    reason: Option<String>,
    updated_at: Option<String>,
    subject: Option<RawSubject>,
    repository: Option<RawRepo>,
}

#[derive(Debug, Deserialize)]
struct RawSubject {
    #[serde(rename = "type")]
    kind: Option<String>,
    title: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRepo {
    name: Option<String>,
    owner: Option<RawOwner>,
    full_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawOwner {
    login: Option<String>,
}

impl RawNotification {
    fn into_notif(self) -> Option<Notification> {
        let subject = self.subject?;
        let kind = subject.kind.unwrap_or_default();
        let url = subject.url.unwrap_or_default();
        let number = url.rsplit('/').next().and_then(|s| s.parse::<i64>().ok());
        let (owner, repo) =
            if let Some(full) = self.repository.as_ref().and_then(|r| r.full_name.clone()) {
                let mut parts = full.splitn(2, '/');
                (parts.next()?.to_string(), parts.next()?.to_string())
            } else {
                let owner = self.repository.as_ref()?.owner.as_ref()?.login.clone()?;
                let repo = self.repository.as_ref()?.name.clone()?;
                (owner, repo)
            };
        Some(Notification {
            id: self.id.unwrap_or_default(),
            unread: self.unread.unwrap_or(false),
            reason: self.reason.unwrap_or_default(),
            updated_at: self.updated_at.unwrap_or_default(),
            subject_type: kind,
            title: subject.title.unwrap_or_default(),
            owner,
            repo,
            number,
        })
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    if let Some(v) = resp.headers().get(header::RETRY_AFTER) {
        if let Ok(s) = v.to_str() {
            if let Ok(secs) = s.parse::<u64>() {
                return Some(Duration::from_secs(secs.min(120)));
            }
        }
    }
    if let Some(v) = resp.headers().get("X-RateLimit-Reset") {
        if let Ok(s) = v.to_str() {
            if let Ok(unix) = s.parse::<i64>() {
                let now = chrono::Utc::now().timestamp();
                let wait = (unix - now).clamp(1, 120) as u64;
                return Some(Duration::from_secs(wait));
            }
        }
    }
    None
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `qualifier` is e.g. `("created", "2026-01-01")` or `("updated", …)`.
/// `None` means no date bound (entire involvement history, GitHub's 1000-hit cap).
pub fn search_queries(
    username: &str,
    qualifier: Option<(&str, &str)>,
) -> Vec<(String, Option<Role>)> {
    let u = quote_user(username);
    let extra = match qualifier {
        Some((key, value)) => format!(" {key}:>={value}"),
        None => String::new(),
    };
    vec![
        (format!("involves:{u}{extra}"), None),
        (
            format!("is:pr reviewed-by:{u}{extra}"),
            Some(Role::Reviewed),
        ),
        (
            format!("is:pr review-requested:{u}{extra}"),
            Some(Role::ReviewRequested),
        ),
    ]
}

fn quote_user(u: &str) -> String {
    if u.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        u.to_string()
    } else {
        format!("\"{u}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_notification_number() {
        let raw = RawNotification {
            id: Some("1".into()),
            unread: Some(true),
            reason: Some("review_requested".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            subject: Some(RawSubject {
                kind: Some("PullRequest".into()),
                title: Some("Fix it".into()),
                url: Some("https://api.github.com/repos/acme/box/pulls/42".into()),
            }),
            repository: Some(RawRepo {
                name: Some("box".into()),
                owner: Some(RawOwner {
                    login: Some("acme".into()),
                }),
                full_name: Some("acme/box".into()),
            }),
        };
        let n = raw.into_notif().unwrap();
        assert_eq!(n.number, Some(42));
        assert_eq!(n.title, "Fix it");
        assert_eq!(n.owner, "acme");
        assert_eq!(n.extra_role(), Some(Role::ReviewRequested));
    }

    #[test]
    fn search_query_count() {
        assert_eq!(
            search_queries("octo", Some(("created", "2026-01-01"))).len(),
            3
        );
        assert!(
            search_queries("octo", Some(("created", "2026-01-01")))[0]
                .0
                .contains("created:>=2026-01-01")
        );
        assert!(
            !search_queries("octo", None)[0].0.contains("created:")
                && !search_queries("octo", None)[0].0.contains("updated:")
        );
    }
}
