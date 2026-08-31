use std::sync::OnceLock;

use regex::Regex;

use crate::model::{IssueLink, LinkKind};

fn repo_num_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)#(\d{1,7})\b").unwrap())
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)https?://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/(?:issues|pull)/(\d{1,7})",
        )
        .unwrap()
    })
}

fn hash_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:^|[\s(\[])#(\d{1,7})\b").unwrap())
}

fn close_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:fix(?:e[sd])?|close[sd]?|resolve[sd]?)\s+(?:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?#(\d{1,7})\b",
        )
        .unwrap()
    })
}

/// Extract issue/PR references from a markdown body without extra API calls.
pub fn extract_refs(body: &str, owner: &str, repo: &str) -> Vec<IssueLink> {
    if body.is_empty() {
        return Vec::new();
    }
    let default_repo = format!("{owner}/{repo}");
    let mut out: Vec<IssueLink> = Vec::new();

    let push = |out: &mut Vec<IssueLink>, repo: String, number: i64, kind: LinkKind| {
        if number <= 0 {
            return;
        }
        if out.iter().any(|l| l.repo == repo && l.number == number) {
            if kind == LinkKind::Closes {
                if let Some(existing) = out
                    .iter_mut()
                    .find(|l| l.repo == repo && l.number == number)
                {
                    existing.kind = LinkKind::Closes;
                }
            }
            return;
        }
        out.push(IssueLink {
            repo,
            number,
            kind,
            title: None,
            state: None,
            to_id: None,
        });
    };

    for cap in url_re().captures_iter(body) {
        let repo = format!("{}/{}", &cap[1], &cap[2]);
        let number: i64 = cap[3].parse().unwrap_or(0);
        push(&mut out, repo, number, LinkKind::Mentioned);
    }
    for cap in repo_num_re().captures_iter(body) {
        let repo = format!("{}/{}", &cap[1], &cap[2]);
        let number: i64 = cap[3].parse().unwrap_or(0);
        push(&mut out, repo, number, LinkKind::Mentioned);
    }
    for cap in close_re().captures_iter(body) {
        let number: i64 = cap[1].parse().unwrap_or(0);
        push(&mut out, default_repo.clone(), number, LinkKind::Closes);
    }
    for cap in hash_re().captures_iter(body) {
        let number: i64 = cap[1].parse().unwrap_or(0);
        push(&mut out, default_repo.clone(), number, LinkKind::Mentioned);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_closes_and_urls() {
        let body =
            "This fixes #12 and see https://github.com/acme/widgets/issues/44 plus other/repo#9";
        let refs = extract_refs(body, "acme", "widgets");
        assert!(
            refs.iter()
                .any(|r| r.number == 12 && r.kind == LinkKind::Closes)
        );
        assert!(
            refs.iter()
                .any(|r| r.repo == "acme/widgets" && r.number == 44)
        );
        assert!(refs.iter().any(|r| r.repo == "other/repo" && r.number == 9));
    }

    #[test]
    fn empty_body() {
        assert!(extract_refs("", "a", "b").is_empty());
    }
}
