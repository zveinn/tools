use serde_json::Value;

use crate::model::{Comment, HydratedItem, IssueLink, ItemState, Kind, Label, LinkKind, Review};
use crate::refs::extract_refs;

pub fn item_from_value(v: &Value) -> Option<HydratedItem> {
    if v.is_null() {
        return None;
    }
    let typename = v.get("__typename").and_then(Value::as_str).unwrap_or("");
    let kind = match typename {
        "PullRequest" => Kind::Pr,
        "Issue" => Kind::Issue,
        _ => return None,
    };

    let number = v.get("number").and_then(Value::as_i64)?;
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(untitled)")
        .to_string();
    let body = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let owner = v
        .pointer("/repository/owner/login")
        .and_then(Value::as_str)?
        .to_string();
    let repo = v
        .pointer("/repository/name")
        .and_then(Value::as_str)?
        .to_string();

    let gql_state = v.get("state").and_then(Value::as_str).unwrap_or("OPEN");
    let merged = v.get("merged").and_then(Value::as_bool).unwrap_or(false);
    let state = if kind == Kind::Pr && (merged || gql_state.eq_ignore_ascii_case("MERGED")) {
        ItemState::Merged
    } else {
        ItemState::parse(gql_state)
    };

    let assignees = nodes_logins(v.pointer("/assignees/nodes"));
    let labels = nodes_labels(v.pointer("/labels/nodes"));
    let review_requests = review_request_logins(v.pointer("/reviewRequests/nodes"));
    let reviews = latest_reviews(v.pointer("/latestReviews/nodes"));
    let links = collect_links(v, &owner, &repo, &body);

    Some(HydratedItem {
        node_id: v.get("id").and_then(Value::as_str).map(str::to_string),
        owner,
        repo,
        number,
        kind,
        title,
        body,
        state,
        author: v
            .pointer("/author/login")
            .and_then(Value::as_str)
            .map(str::to_string),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        html_url: v
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        created_at: v
            .get("createdAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        updated_at: v
            .get("updatedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        closed_at: v
            .get("closedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        merged_at: v
            .get("mergedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        comments_count: v
            .pointer("/comments/totalCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        review_decision: v
            .get("reviewDecision")
            .and_then(Value::as_str)
            .map(str::to_string),
        additions: v.get("additions").and_then(Value::as_i64),
        deletions: v.get("deletions").and_then(Value::as_i64),
        changed_files: v.get("changedFiles").and_then(Value::as_i64),
        assignees,
        labels,
        review_requests,
        reviews,
        links,
    })
}

pub fn comments_from_value(v: &Value) -> Vec<Comment> {
    let mut out = Vec::new();
    if let Some(nodes) = v.pointer("/comments/nodes").and_then(Value::as_array) {
        for n in nodes {
            out.push(Comment {
                github_id: n.get("databaseId").and_then(Value::as_i64),
                kind: "issue_comment".into(),
                author: n
                    .pointer("/author/login")
                    .and_then(Value::as_str)
                    .unwrap_or("ghost")
                    .to_string(),
                body: n
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                created_at: n
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    if let Some(nodes) = v.pointer("/latestReviews/nodes").and_then(Value::as_array) {
        for n in nodes {
            let body = n.get("body").and_then(Value::as_str).unwrap_or("");
            if body.trim().is_empty() {
                continue;
            }
            out.push(Comment {
                github_id: n.get("databaseId").and_then(Value::as_i64),
                kind: "review".into(),
                author: n
                    .pointer("/author/login")
                    .and_then(Value::as_str)
                    .unwrap_or("ghost")
                    .to_string(),
                body: body.to_string(),
                created_at: n
                    .get("submittedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

fn collect_links(v: &Value, owner: &str, repo: &str, body: &str) -> Vec<IssueLink> {
    let mut links = Vec::new();
    push_ref_nodes(
        &mut links,
        v.pointer("/closingIssuesReferences/nodes"),
        LinkKind::Closes,
    );
    push_ref_nodes(
        &mut links,
        v.pointer("/closedByPullRequestsReferences/nodes"),
        LinkKind::Closes,
    );

    if let Some(nodes) = v.pointer("/timelineItems/nodes").and_then(Value::as_array) {
        for n in nodes {
            match n.get("__typename").and_then(Value::as_str) {
                Some("CrossReferencedEvent") => {
                    let kind = if n.get("willCloseTarget").and_then(Value::as_bool) == Some(true) {
                        LinkKind::Closes
                    } else {
                        LinkKind::Mentioned
                    };
                    if let Some(src) = n.get("source") {
                        push_node(&mut links, src, kind);
                    }
                }
                Some("ConnectedEvent") => {
                    if let Some(sub) = n.get("subject") {
                        push_node(&mut links, sub, LinkKind::Mentioned);
                    }
                }
                _ => {}
            }
        }
    }

    let mut texts = vec![body.to_string()];
    for path in ["/comments/nodes", "/latestReviews/nodes"] {
        if let Some(nodes) = v.pointer(path).and_then(Value::as_array) {
            for n in nodes {
                if let Some(b) = n.get("body").and_then(Value::as_str) {
                    texts.push(b.to_string());
                }
            }
        }
    }
    for text in texts {
        for extra in extract_refs(&text, owner, repo) {
            merge_link(&mut links, extra);
        }
    }
    links
}

fn push_ref_nodes(links: &mut Vec<IssueLink>, nodes: Option<&Value>, kind: LinkKind) {
    let Some(arr) = nodes.and_then(Value::as_array) else {
        return;
    };
    for n in arr {
        push_node(links, n, kind);
    }
}

fn push_node(links: &mut Vec<IssueLink>, n: &Value, kind: LinkKind) {
    if n.is_null() {
        return;
    }
    let typename = n.get("__typename").and_then(Value::as_str).unwrap_or("");
    if !typename.is_empty() && typename != "Issue" && typename != "PullRequest" {
        return;
    }
    let Some(number) = n.get("number").and_then(Value::as_i64) else {
        return;
    };
    let repo = n
        .pointer("/repository/nameWithOwner")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if repo.is_empty() {
        return;
    }
    merge_link(
        links,
        IssueLink {
            repo,
            number,
            kind,
            title: n.get("title").and_then(Value::as_str).map(str::to_string),
            state: n.get("state").and_then(Value::as_str).map(ItemState::parse),
            to_id: None,
        },
    );
}

fn merge_link(links: &mut Vec<IssueLink>, new: IssueLink) {
    if let Some(existing) = links
        .iter_mut()
        .find(|l| l.repo.eq_ignore_ascii_case(&new.repo) && l.number == new.number)
    {
        if new.kind == LinkKind::Closes {
            existing.kind = LinkKind::Closes;
        }
        if existing.title.is_none() {
            existing.title = new.title;
        }
        if existing.state.is_none() {
            existing.state = new.state;
        }
        return;
    }
    links.push(new);
}

fn nodes_logins(nodes: Option<&Value>) -> Vec<String> {
    let Some(arr) = nodes.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|n| n.get("login").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn nodes_labels(nodes: Option<&Value>) -> Vec<Label> {
    let Some(arr) = nodes.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|n| {
            Some(Label {
                name: n.get("name").and_then(Value::as_str)?.to_string(),
                color: n
                    .get("color")
                    .and_then(Value::as_str)
                    .unwrap_or("888888")
                    .to_string(),
            })
        })
        .collect()
}

fn review_request_logins(nodes: Option<&Value>) -> Vec<String> {
    let Some(arr) = nodes.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in arr {
        let reviewer = match n.get("requestedReviewer") {
            Some(v) if !v.is_null() => v,
            _ => continue,
        };
        if let Some(login) = reviewer.get("login").and_then(Value::as_str) {
            out.push(login.to_string());
        } else if let Some(slug) = reviewer.get("combinedSlug").and_then(Value::as_str) {
            out.push(slug.to_string());
        }
    }
    out
}

fn latest_reviews(nodes: Option<&Value>) -> Vec<Review> {
    let Some(arr) = nodes.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .map(|n| Review {
            github_id: n.get("databaseId").and_then(Value::as_i64),
            author: n
                .pointer("/author/login")
                .and_then(Value::as_str)
                .unwrap_or("ghost")
                .to_string(),
            state: n
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            submitted_at: n
                .get("submittedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            body: n
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_pr() {
        let v = json!({
            "__typename": "PullRequest",
            "id": "PR_1",
            "number": 7,
            "title": "Fix it",
            "body": "closes #3",
            "state": "OPEN",
            "merged": false,
            "isDraft": true,
            "url": "https://github.com/acme/box/pull/7",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "author": { "login": "alice" },
            "reviewDecision": "CHANGES_REQUESTED",
            "additions": 4,
            "deletions": 1,
            "changedFiles": 2,
            "comments": { "totalCount": 3 },
            "assignees": { "nodes": [{ "login": "bob" }] },
            "labels": { "nodes": [{ "name": "bug", "color": "ff0000" }] },
            "reviewRequests": { "nodes": [{
                "requestedReviewer": { "__typename": "User", "login": "me" }
            }]},
            "latestReviews": { "nodes": [{
                "databaseId": 9,
                "author": { "login": "carol" },
                "state": "APPROVED",
                "submittedAt": "2026-01-02T00:00:00Z",
                "body": "lgtm"
            }]},
            "closingIssuesReferences": { "nodes": [{
                "number": 3,
                "title": "orig",
                "state": "OPEN",
                "url": "https://github.com/acme/box/issues/3",
                "repository": { "nameWithOwner": "acme/box" }
            }]},
            "repository": { "owner": { "login": "acme" }, "name": "box" }
        });
        let item = item_from_value(&v).unwrap();
        assert_eq!(item.kind, Kind::Pr);
        assert_eq!(item.number, 7);
        assert!(item.draft);
        assert_eq!(item.review_requests, vec!["me"]);
        assert_eq!(item.reviews.len(), 1);
        assert!(
            item.links
                .iter()
                .any(|l| l.number == 3 && l.kind == LinkKind::Closes)
        );
        let roles = item.field_roles("me");
        assert!(roles.contains(&crate::model::Role::ReviewRequested));
        assert!(!roles.contains(&crate::model::Role::Authored));
    }

    #[test]
    fn links_from_timeline_and_comment_url() {
        let v = json!({
            "__typename": "PullRequest",
            "id": "PR_9",
            "number": 6752,
            "title": "metrics",
            "body": "no issue number in the description",
            "state": "OPEN",
            "url": "https://github.com/miniohq/aistor/pull/6752",
            "comments": {
                "totalCount": 1,
                "nodes": [{ "body": "https://github.com/miniohq/aistor/issues/6671" }]
            },
            "closingIssuesReferences": { "nodes": [{
                "number": 6671,
                "title": "memory analytics",
                "state": "OPEN",
                "repository": { "nameWithOwner": "miniohq/aistor" }
            }]},
            "timelineItems": { "nodes": [
                {
                    "__typename": "ConnectedEvent",
                    "subject": {
                        "__typename": "Issue",
                        "number": 6671,
                        "title": "memory analytics",
                        "state": "OPEN",
                        "repository": { "nameWithOwner": "miniohq/aistor" }
                    }
                }
            ]},
            "repository": { "owner": { "login": "miniohq" }, "name": "aistor" }
        });
        let item = item_from_value(&v).unwrap();
        let hits: Vec<_> = item.links.iter().filter(|l| l.number == 6671).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, LinkKind::Closes);
        assert_eq!(hits[0].title.as_deref(), Some("memory analytics"));
    }

    #[test]
    fn parse_merged() {
        let v = json!({
            "__typename": "PullRequest",
            "id": "PR_2",
            "number": 1,
            "title": "done",
            "body": "",
            "state": "MERGED",
            "merged": true,
            "url": "https://github.com/a/b/pull/1",
            "repository": { "owner": { "login": "a" }, "name": "b" }
        });
        let item = item_from_value(&v).unwrap();
        assert_eq!(item.state, ItemState::Merged);
    }
}
