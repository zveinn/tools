use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{
    Comment, HydratedItem, InboxRow, IssueLink, ItemDetail, ItemQuery, ItemRow, ItemState, Kind,
    Label, LinkKind, Review, Role, StateFilter, View, review_progress,
};
use crate::timeutil::now_rfc3339;

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
    pub path: PathBuf,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.inner.lock().expect("db mutex poisoned");
        f(&conn)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .context("meta_get")
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }

    pub fn upsert_item(
        &self,
        item: &HydratedItem,
        extra_roles: &BTreeSet<Role>,
        mark_unread: bool,
        replace_field_roles: bool,
    ) -> Result<i64> {
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let repo_id = {
                tx.execute(
                    "INSERT INTO repos(owner, name) VALUES (?1, ?2)
                     ON CONFLICT(owner, name) DO NOTHING",
                    params![item.owner, item.repo],
                )?;
                tx.query_row(
                    "SELECT id FROM repos WHERE owner = ?1 AND name = ?2",
                    params![item.owner, item.repo],
                    |r| r.get::<_, i64>(0),
                )?
            };

            let node_id = item
                .node_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let by_node: Option<i64> = if let Some(nid) = node_id {
                tx.query_row(
                    "SELECT id FROM items WHERE node_id = ?1",
                    params![nid],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };
            let by_key: Option<i64> = tx
                .query_row(
                    "SELECT id FROM items WHERE repo_id = ?1 AND number = ?2",
                    params![repo_id, item.number],
                    |r| r.get(0),
                )
                .optional()?;

            let id = match (by_node, by_key) {
                (Some(a), Some(b)) if a != b => {
                    // Same GitHub node, different repo/number (rename or transfer).
                    merge_item_into(&tx, b, a)?;
                    b
                }
                (Some(a), _) => a,
                (None, Some(b)) => b,
                (None, None) => {
                    tx.execute(
                        "INSERT INTO items (repo_id, number, kind, title, state, unread)
                         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                        params![
                            repo_id,
                            item.number,
                            item.kind.as_str(),
                            item.title,
                            item.state.as_str()
                        ],
                    )?;
                    tx.last_insert_rowid()
                }
            };

            let prior: Option<(Option<String>, i64)> = match (by_node, by_key) {
                (None, None) => None,
                _ => tx
                    .query_row(
                        "SELECT updated_at, unread FROM items WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
            };
            let newer = match (&prior, &item.updated_at) {
                (Some((Some(prev), _)), Some(next)) => next.as_str() > prev.as_str(),
                (None, _) => true,
                _ => item.updated_at.is_some(),
            };
            let unread = if mark_unread && newer {
                1
            } else {
                prior.map(|(_, u)| u).unwrap_or(0)
            };

            tx.execute(
                "UPDATE items SET
                    repo_id = ?1,
                    number = ?2,
                    node_id = COALESCE(?3, node_id),
                    kind = ?4,
                    title = ?5,
                    body = ?6,
                    state = ?7,
                    author = ?8,
                    draft = ?9,
                    html_url = ?10,
                    created_at = COALESCE(?11, created_at),
                    updated_at = ?12,
                    closed_at = ?13,
                    merged_at = ?14,
                    comments_count = ?15,
                    review_decision = ?16,
                    additions = ?17,
                    deletions = ?18,
                    changed_files = ?19,
                    unread = ?20,
                    last_hydrated_at = ?21
                 WHERE id = ?22",
                params![
                    repo_id,
                    item.number,
                    node_id,
                    item.kind.as_str(),
                    item.title,
                    item.body,
                    item.state.as_str(),
                    item.author,
                    item.draft as i64,
                    item.html_url,
                    item.created_at,
                    item.updated_at,
                    item.closed_at,
                    item.merged_at,
                    item.comments_count,
                    item.review_decision,
                    item.additions,
                    item.deletions,
                    item.changed_files,
                    unread,
                    now_rfc3339(),
                    id,
                ],
            )?;

            if replace_field_roles {
                tx.execute(
                    "DELETE FROM item_roles WHERE item_id = ?1 AND role IN
                     ('authored','assigned','reviewed','review_requested')",
                    params![id],
                )?;
            }

            let roles = extra_roles;
            for role in roles {
                tx.execute(
                    "INSERT OR IGNORE INTO item_roles(item_id, role) VALUES (?1, ?2)",
                    params![id, role.as_str()],
                )?;
            }

            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM item_roles WHERE item_id = ?1 AND role != 'involved'",
                params![id],
                |r| r.get(0),
            )?;
            if count > 0 {
                tx.execute(
                    "DELETE FROM item_roles WHERE item_id = ?1 AND role = 'involved'",
                    params![id],
                )?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO item_roles(item_id, role) VALUES (?1, 'involved')",
                    params![id],
                )?;
            }

            tx.execute("DELETE FROM item_labels WHERE item_id = ?1", params![id])?;
            for label in &item.labels {
                tx.execute(
                    "INSERT OR IGNORE INTO item_labels(item_id, name, color) VALUES (?1, ?2, ?3)",
                    params![id, label.name, label.color],
                )?;
            }

            tx.execute("DELETE FROM item_assignees WHERE item_id = ?1", params![id])?;
            for login in &item.assignees {
                tx.execute(
                    "INSERT OR IGNORE INTO item_assignees(item_id, login) VALUES (?1, ?2)",
                    params![id, login],
                )?;
            }

            tx.execute("DELETE FROM reviews WHERE item_id = ?1", params![id])?;
            for rev in &item.reviews {
                tx.execute(
                    "INSERT INTO reviews(item_id, github_id, author, state, submitted_at, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id,
                        rev.github_id,
                        rev.author,
                        rev.state,
                        rev.submitted_at,
                        rev.body
                    ],
                )?;
            }

            tx.execute(
                "DELETE FROM item_review_requests WHERE item_id = ?1",
                params![id],
            )?;
            for login in &item.review_requests {
                tx.execute(
                    "INSERT OR IGNORE INTO item_review_requests(item_id, login) VALUES (?1, ?2)",
                    params![id, login],
                )?;
            }

            tx.execute("DELETE FROM links WHERE from_id = ?1", params![id])?;
            let self_repo = format!("{}/{}", item.owner, item.repo);
            for link in &item.links {
                let to_id: Option<i64> = tx
                    .query_row(
                        "SELECT i.id FROM items i
                         JOIN repos r ON r.id = i.repo_id
                         WHERE lower(r.owner || '/' || r.name) = lower(?1) AND i.number = ?2",
                        params![link.repo, link.number],
                        |r| r.get(0),
                    )
                    .optional()?;
                tx.execute(
                    "INSERT OR IGNORE INTO links(from_id, to_repo, to_number, to_id, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, link.repo, link.number, to_id, link.kind.as_str()],
                )?;
                // Hydrating either side should nest the issue under the PR.
                if let Some(other_id) = to_id {
                    tx.execute(
                        "INSERT OR IGNORE INTO links(from_id, to_repo, to_number, to_id, kind)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![other_id, self_repo, item.number, id, link.kind.as_str()],
                    )?;
                }
            }

            tx.commit()?;
            Ok(id)
        })
    }

    pub fn add_role(&self, item_id: i64, role: Role) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO item_roles(item_id, role) VALUES (?1, ?2)",
                params![item_id, role.as_str()],
            )?;
            if role != Role::Involved {
                c.execute(
                    "DELETE FROM item_roles WHERE item_id = ?1 AND role = 'involved'",
                    params![item_id],
                )?;
            }
            Ok(())
        })
    }

    pub fn set_unread(&self, id: i64, unread: bool) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE items SET unread = ?1 WHERE id = ?2",
                params![unread as i64, id],
            )?;
            Ok(())
        })
    }

    pub fn upsert_notification(
        &self,
        github_id: &str,
        unread: bool,
        reason: &str,
        updated_at: &str,
        subject_type: &str,
        title: &str,
        owner: &str,
        repo: &str,
        number: Option<i64>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO notifications(
                    github_id, unread, reason, updated_at, subject_type, title,
                    owner, repo, number
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(github_id) DO UPDATE SET
                    unread = excluded.unread,
                    reason = excluded.reason,
                    updated_at = excluded.updated_at,
                    subject_type = excluded.subject_type,
                    title = excluded.title,
                    owner = excluded.owner,
                    repo = excluded.repo,
                    number = excluded.number",
                params![
                    github_id,
                    unread as i64,
                    reason,
                    updated_at,
                    subject_type,
                    title,
                    owner,
                    repo,
                    number
                ],
            )?;
            Ok(())
        })
    }

    pub fn bind_notification_item(
        &self,
        owner: &str,
        repo: &str,
        number: i64,
        item_id: i64,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE notifications SET item_id = ?1
                 WHERE owner = ?2 AND repo = ?3 AND number = ?4",
                params![item_id, owner, repo, number],
            )?;
            Ok(())
        })
    }

    pub fn set_notif_unread(&self, github_id: &str, unread: bool) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE notifications SET unread = ?1 WHERE github_id = ?2",
                params![unread as i64, github_id],
            )?;
            Ok(())
        })
    }

    pub fn count_notifications(&self, search: &str, allowed_repos: &[String]) -> Result<usize> {
        self.with(|c| {
            let (inner, args) = inbox_union_sql(search, allowed_repos);
            let sql = format!("SELECT COUNT(*) FROM ({inner})");
            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|a| a.as_ref()).collect();
            let n: i64 = stmt.query_row(refs.as_slice(), |r| r.get(0))?;
            Ok(n as usize)
        })
    }

    pub fn list_notifications(
        &self,
        search: &str,
        allowed_repos: &[String],
    ) -> Result<Vec<InboxRow>> {
        self.with(|c| {
            let (inner, args) = inbox_union_sql(search, allowed_repos);
            let sql = format!("{inner} ORDER BY updated_at DESC");
            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|a| a.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |r| {
                Ok(InboxRow {
                    github_id: r.get(0)?,
                    unread: r.get::<_, i64>(1)? != 0,
                    reason: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    updated_at: r.get(3)?,
                    subject_type: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    owner: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    repo: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    number: r.get(7)?,
                    title: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    item_id: r.get(9)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn find_item_id(&self, owner: &str, repo: &str, number: i64) -> Result<Option<i64>> {
        self.with(|c| {
            c.query_row(
                "SELECT i.id FROM items i
                 JOIN repos r ON r.id = i.repo_id
                 WHERE r.owner = ?1 AND r.name = ?2 AND i.number = ?3",
                params![owner, repo, number],
                |r| r.get(0),
            )
            .optional()
            .context("find_item_id")
        })
    }

    pub fn replace_comments(&self, item_id: i64, comments: &[Comment]) -> Result<()> {
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM comments WHERE item_id = ?1", params![item_id])?;
            for cm in comments {
                tx.execute(
                    "INSERT INTO comments(item_id, github_id, kind, author, body, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        item_id,
                        cm.github_id,
                        cm.kind,
                        cm.author,
                        cm.body,
                        cm.created_at
                    ],
                )?;
            }
            tx.execute(
                "UPDATE items SET comments_fetched_at = ?1, comments_count = ?2 WHERE id = ?3",
                params![now_rfc3339(), comments.len() as i64, item_id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn list(&self, q: &ItemQuery) -> Result<Vec<ItemRow>> {
        self.with(|c| {
            let mut sql = String::from(
                "SELECT i.id, r.owner, r.name, i.number, i.kind, i.title, i.state, i.author,
                        i.draft, i.html_url, i.updated_at, i.review_decision, i.additions,
                        i.deletions, i.unread
                 FROM items i
                 JOIN repos r ON r.id = i.repo_id
                 WHERE 1=1",
            );
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            apply_view_filter(&mut sql, q.view);
            apply_state_filter(&mut sql, q);
            apply_time_filter(&mut sql, q, &mut args);
            apply_repo_filter(&mut sql, q, &mut args);
            apply_search_filter(&mut sql, q, &mut args);

            sql.push_str(" ORDER BY i.updated_at DESC");

            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|a| a.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |row| {
                Ok(ItemRow {
                    id: row.get(0)?,
                    owner: row.get(1)?,
                    repo: row.get(2)?,
                    number: row.get(3)?,
                    kind: Kind::parse(&row.get::<_, String>(4)?).unwrap_or(Kind::Issue),
                    title: row.get(5)?,
                    state: ItemState::parse(&row.get::<_, String>(6)?),
                    author: row.get(7)?,
                    draft: row.get::<_, i64>(8)? != 0,
                    html_url: row.get(9)?,
                    updated_at: row.get(10)?,
                    review_decision: row.get(11)?,
                    additions: row.get(12)?,
                    deletions: row.get(13)?,
                    unread: row.get::<_, i64>(14)? != 0,
                    roles: BTreeSet::new(),
                    approvals: 0,
                    review_total: 0,
                    links: Vec::new(),
                })
            })?;

            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            drop(stmt);

            if !items.is_empty() {
                let ids: Vec<String> = items.iter().map(|i| i.id.to_string()).collect();
                let in_list = ids.join(",");
                let role_sql =
                    format!("SELECT item_id, role FROM item_roles WHERE item_id IN ({in_list})");
                let mut stmt = c.prepare(&role_sql)?;
                let mut rows = stmt.query([])?;
                let mut map: std::collections::HashMap<i64, BTreeSet<Role>> =
                    std::collections::HashMap::new();
                while let Some(row) = rows.next()? {
                    let id: i64 = row.get(0)?;
                    let role_s: String = row.get(1)?;
                    if let Some(role) = Role::parse(&role_s) {
                        map.entry(id).or_default().insert(role);
                    }
                }
                for item in &mut items {
                    item.roles = map.remove(&item.id).unwrap_or_default();
                }
                attach_review_progress(c, &mut items)?;
                attach_links(c, &mut items)?;
            }
            Ok(items)
        })
    }

    pub fn count(&self, q: &ItemQuery) -> Result<usize> {
        self.with(|c| {
            let mut sql = String::from(
                "SELECT COUNT(*) FROM items i JOIN repos r ON r.id = i.repo_id WHERE 1=1",
            );
            let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            apply_view_filter(&mut sql, q.view);
            apply_state_filter(&mut sql, q);
            apply_time_filter(&mut sql, q, &mut args);
            apply_repo_filter(&mut sql, q, &mut args);
            apply_search_filter(&mut sql, q, &mut args);
            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|a| a.as_ref()).collect();
            let n: i64 = stmt.query_row(refs.as_slice(), |r| r.get(0))?;
            Ok(n as usize)
        })
    }

    pub fn counts_by_view(&self, base: &ItemQuery) -> Result<Vec<(View, usize)>> {
        let mut out = Vec::new();
        for view in View::ALL {
            if view == View::Inbox {
                out.push((
                    view,
                    self.count_notifications(&base.search, &base.allowed_repos)?,
                ));
                continue;
            }
            let mut q = base.clone();
            q.view = view;
            if !view.uses_state_filter() {
                q.state = StateFilter::All;
            }
            out.push((view, self.count(&q)?));
        }
        Ok(out)
    }

    pub fn get_detail(&self, id: i64) -> Result<Option<ItemDetail>> {
        self.with(|c| {
            let row = c
                .query_row(
                    "SELECT i.id, r.owner, r.name, i.number, i.kind, i.title, i.state, i.author,
                            i.draft, i.html_url, i.updated_at, i.review_decision, i.additions,
                            i.deletions, i.unread, i.body, i.created_at, i.closed_at, i.merged_at,
                            i.comments_count, i.changed_files, i.comments_fetched_at
                     FROM items i JOIN repos r ON r.id = i.repo_id WHERE i.id = ?1",
                    params![id],
                    |r| {
                        Ok((
                            ItemRow {
                                id: r.get(0)?,
                                owner: r.get(1)?,
                                repo: r.get(2)?,
                                number: r.get(3)?,
                                kind: Kind::parse(&r.get::<_, String>(4)?).unwrap_or(Kind::Issue),
                                title: r.get(5)?,
                                state: ItemState::parse(&r.get::<_, String>(6)?),
                                author: r.get(7)?,
                                draft: r.get::<_, i64>(8)? != 0,
                                html_url: r.get(9)?,
                                updated_at: r.get(10)?,
                                review_decision: r.get(11)?,
                                additions: r.get(12)?,
                                deletions: r.get(13)?,
                                unread: r.get::<_, i64>(14)? != 0,
                                roles: BTreeSet::new(),
                                approvals: 0,
                                review_total: 0,
                                links: Vec::new(),
                            },
                            r.get::<_, String>(15)?,
                            r.get::<_, Option<String>>(16)?,
                            r.get::<_, Option<String>>(17)?,
                            r.get::<_, Option<String>>(18)?,
                            r.get::<_, i64>(19)?,
                            r.get::<_, Option<i64>>(20)?,
                            r.get::<_, Option<String>>(21)?,
                        ))
                    },
                )
                .optional()?;

            let Some((mut row, body, created_at, closed_at, merged_at, comments_count, changed_files, comments_fetched_at)) =
                row
            else {
                return Ok(None);
            };

            let mut roles = BTreeSet::new();
            let mut stmt = c.prepare("SELECT role FROM item_roles WHERE item_id = ?1")?;
            let rs = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
            for r in rs {
                if let Some(role) = Role::parse(&r?) {
                    roles.insert(role);
                }
            }
            row.roles = roles;

            let mut labels = Vec::new();
            let mut stmt = c.prepare("SELECT name, color FROM item_labels WHERE item_id = ?1")?;
            let rs = stmt.query_map(params![id], |r| {
                Ok(Label {
                    name: r.get(0)?,
                    color: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                })
            })?;
            for l in rs {
                labels.push(l?);
            }

            let mut assignees = Vec::new();
            let mut stmt = c.prepare("SELECT login FROM item_assignees WHERE item_id = ?1")?;
            let rs = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
            for a in rs {
                assignees.push(a?);
            }

            let mut reviews = Vec::new();
            let mut stmt = c.prepare(
                "SELECT github_id, author, state, submitted_at, body FROM reviews WHERE item_id = ?1",
            )?;
            let rs = stmt.query_map(params![id], |r| {
                Ok(Review {
                    github_id: r.get(0)?,
                    author: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    state: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    submitted_at: r.get(3)?,
                    body: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                })
            })?;
            for rev in rs {
                reviews.push(rev?);
            }

            let mut requested = Vec::new();
            let mut stmt =
                c.prepare("SELECT login FROM item_review_requests WHERE item_id = ?1")?;
            let rs = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
            for login in rs {
                requested.push(login?);
            }
            let (approvals, review_total) = review_progress(&reviews, &requested);
            row.approvals = approvals;
            row.review_total = review_total;

            let mut comments = Vec::new();
            let mut stmt = c.prepare(
                "SELECT github_id, kind, author, body, created_at FROM comments
                 WHERE item_id = ?1 ORDER BY created_at ASC",
            )?;
            let rs = stmt.query_map(params![id], |r| {
                Ok(Comment {
                    github_id: r.get(0)?,
                    kind: r.get(1)?,
                    author: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    body: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    created_at: r.get(4)?,
                })
            })?;
            for cm in rs {
                comments.push(cm?);
            }

            let mut links = Vec::new();
            let mut stmt = c.prepare(
                "SELECT l.to_repo, l.to_number, l.kind, i.title, i.state, l.to_id
                 FROM links l
                 LEFT JOIN items i ON i.id = l.to_id
                 WHERE l.from_id = ?1
                 ORDER BY CASE l.kind WHEN 'closes' THEN 0 ELSE 1 END, l.to_number",
            )?;
            let rs = stmt.query_map(params![id], |r| {
                Ok(IssueLink {
                    repo: r.get(0)?,
                    number: r.get(1)?,
                    kind: LinkKind::parse(&r.get::<_, String>(2)?),
                    title: r.get(3)?,
                    state: r
                        .get::<_, Option<String>>(4)?
                        .map(|s| ItemState::parse(&s)),
                    to_id: r.get(5)?,
                })
            })?;
            for l in rs {
                links.push(l?);
            }

            Ok(Some(ItemDetail {
                row,
                body,
                created_at,
                closed_at,
                merged_at,
                comments_count,
                changed_files,
                labels,
                assignees,
                reviews,
                comments,
                links,
                comments_fetched_at,
            }))
        })
    }

    pub fn open_stale(
        &self,
        older_than: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, i64, Option<String>)>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT r.owner, r.name, i.number, i.node_id
                 FROM items i JOIN repos r ON r.id = i.repo_id
                 WHERE i.state = 'open'
                   AND (i.last_hydrated_at IS NULL OR i.last_hydrated_at < ?1)
                 ORDER BY i.last_hydrated_at IS NULL DESC, i.updated_at ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![older_than, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn stats(&self) -> Result<DbStats> {
        self.with(|c| {
            let prs: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE kind = 'pr'", [], |r| {
                    r.get(0)
                })?;
            let issues: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE kind = 'issue'", [], |r| {
                    r.get(0)
                })?;
            let open: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE state = 'open'", [], |r| {
                    r.get(0)
                })?;
            let unread: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE unread = 1", [], |r| {
                    r.get(0)
                })?;
            let comments: i64 = c.query_row("SELECT COUNT(*) FROM comments", [], |r| r.get(0))?;
            Ok(DbStats {
                prs,
                issues,
                open,
                unread,
                comments,
            })
        })
    }
}

#[derive(Debug, Clone)]
pub struct DbStats {
    pub prs: i64,
    pub issues: i64,
    pub open: i64,
    pub unread: i64,
    pub comments: i64,
}

fn migrate(conn: &Connection) -> Result<()> {
    let version = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| "0".into());
    let v: i64 = version.parse().unwrap_or(0);
    if v < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS item_review_requests (
                item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                login   TEXT NOT NULL,
                PRIMARY KEY (item_id, login)
            );
            UPDATE items SET last_hydrated_at = NULL WHERE kind = 'pr' AND state = 'open';
            INSERT INTO meta(key, value) VALUES ('schema_version', '2')
            ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
        )?;
    }
    if v < 3 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notifications (
                github_id    TEXT PRIMARY KEY,
                unread       INTEGER NOT NULL DEFAULT 0,
                reason       TEXT,
                updated_at   TEXT,
                subject_type TEXT,
                owner        TEXT,
                repo         TEXT,
                number       INTEGER,
                title        TEXT,
                item_id      INTEGER REFERENCES items(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_notif_updated ON notifications(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_notif_unread  ON notifications(unread);
            INSERT INTO meta(key, value) VALUES ('schema_version', '3')
            ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
        )?;
    }
    Ok(())
}

fn attach_review_progress(c: &Connection, items: &mut [ItemRow]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = items.iter().map(|i| i.id.to_string()).collect();
    let in_list = ids.join(",");

    let mut reviews_by: std::collections::HashMap<i64, Vec<Review>> =
        std::collections::HashMap::new();
    let mut stmt = c.prepare(&format!(
        "SELECT item_id, author, state FROM reviews WHERE item_id IN ({in_list})"
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        reviews_by.entry(id).or_default().push(Review {
            github_id: None,
            author: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            state: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            submitted_at: None,
            body: String::new(),
        });
    }

    let mut req_by: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    let mut stmt = c.prepare(&format!(
        "SELECT item_id, login FROM item_review_requests WHERE item_id IN ({in_list})"
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        req_by.entry(id).or_default().push(row.get(1)?);
    }

    for item in items {
        let reviews = reviews_by.remove(&item.id).unwrap_or_default();
        let requested = req_by.remove(&item.id).unwrap_or_default();
        let (approvals, review_total) = review_progress(&reviews, &requested);
        item.approvals = approvals;
        item.review_total = review_total;
    }
    Ok(())
}

fn attach_links(c: &Connection, items: &mut [ItemRow]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = items.iter().map(|i| i.id.to_string()).collect();
    let in_list = ids.join(",");
    let mut map: std::collections::HashMap<i64, Vec<IssueLink>> = std::collections::HashMap::new();
    let mut stmt = c.prepare(&format!(
        "SELECT l.from_id, l.to_repo, l.to_number, l.kind, i.title, i.state, l.to_id
         FROM links l
         LEFT JOIN items i ON i.id = l.to_id
         WHERE l.from_id IN ({in_list})
         ORDER BY CASE l.kind WHEN 'closes' THEN 0 ELSE 1 END, l.to_number"
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        map.entry(id).or_default().push(IssueLink {
            repo: row.get(1)?,
            number: row.get(2)?,
            kind: LinkKind::parse(&row.get::<_, String>(3)?),
            title: row.get(4)?,
            state: row
                .get::<_, Option<String>>(5)?
                .map(|s| ItemState::parse(&s)),
            to_id: row.get(6)?,
        });
    }
    for item in items {
        item.links = map.remove(&item.id).unwrap_or_default();
    }
    Ok(())
}

/// Move child rows from `drop_id` onto `keep_id`, then delete `drop_id`.
/// Used when GitHub renames a repo or transfers an issue/PR (same node_id,
/// new owner/repo/number).
fn merge_item_into(tx: &Connection, keep_id: i64, drop_id: i64) -> Result<()> {
    if keep_id == drop_id {
        return Ok(());
    }
    tracing::info!(keep_id, drop_id, "merging transferred GitHub item");
    tx.execute(
        "INSERT OR IGNORE INTO item_roles(item_id, role)
         SELECT ?1, role FROM item_roles WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO item_labels(item_id, name, color)
         SELECT ?1, name, color FROM item_labels WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO item_assignees(item_id, login)
         SELECT ?1, login FROM item_assignees WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO item_review_requests(item_id, login)
         SELECT ?1, login FROM item_review_requests WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "UPDATE reviews SET item_id = ?1 WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO comments(item_id, github_id, kind, author, body, created_at)
         SELECT ?1, github_id, kind, author, body, created_at
         FROM comments WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO links(from_id, to_repo, to_number, to_id, kind)
         SELECT ?1, to_repo, to_number, to_id, kind FROM links WHERE from_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "UPDATE links SET to_id = ?1 WHERE to_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute(
        "UPDATE notifications SET item_id = ?1 WHERE item_id = ?2",
        params![keep_id, drop_id],
    )?;
    tx.execute("DELETE FROM items WHERE id = ?1", params![drop_id])?;
    Ok(())
}

/// GitHub notification threads plus open items waiting on you
/// (review requested, assigned-but-not-authored, mentioned, unread).
fn inbox_union_sql(
    search: &str,
    allowed_repos: &[String],
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let mut notifs = String::from(
        "SELECT github_id, unread, reason, updated_at, subject_type, owner, repo,
                number, title, item_id
         FROM notifications WHERE 1=1",
    );
    apply_inbox_repo_filter(&mut notifs, "owner", "repo", allowed_repos, &mut args);
    apply_inbox_search(
        &mut notifs,
        "title",
        "reason",
        "owner",
        "repo",
        "number",
        search,
        &mut args,
    );

    let mut items = String::from(
        "SELECT printf('local:%d', i.id) AS github_id,
                i.unread,
                CASE
                  WHEN EXISTS (
                    SELECT 1 FROM item_roles ir
                    WHERE ir.item_id = i.id AND ir.role = 'review_requested'
                  ) THEN 'review_requested'
                  WHEN EXISTS (
                    SELECT 1 FROM item_roles ir
                    WHERE ir.item_id = i.id AND ir.role = 'assigned'
                  ) THEN 'assign'
                  WHEN EXISTS (
                    SELECT 1 FROM item_roles ir
                    WHERE ir.item_id = i.id AND ir.role = 'mentioned'
                  ) THEN 'mention'
                  ELSE 'subscribed'
                END AS reason,
                i.updated_at,
                CASE i.kind WHEN 'pr' THEN 'PullRequest' ELSE 'Issue' END AS subject_type,
                r.owner, r.name AS repo, i.number, i.title, i.id AS item_id
         FROM items i
         JOIN repos r ON r.id = i.repo_id
         WHERE i.state = 'open'
           AND (
             i.unread = 1
             OR i.id IN (
               SELECT item_id FROM item_roles
               WHERE role IN ('review_requested', 'mentioned')
             )
             OR (
               i.id IN (SELECT item_id FROM item_roles WHERE role = 'assigned')
               AND i.id NOT IN (SELECT item_id FROM item_roles WHERE role = 'authored')
             )
           )
           AND NOT EXISTS (
             SELECT 1 FROM notifications n
             WHERE n.item_id = i.id
                OR (n.owner = r.owner AND n.repo = r.name AND n.number = i.number)
           )",
    );
    apply_inbox_repo_filter(&mut items, "r.owner", "r.name", allowed_repos, &mut args);
    apply_inbox_search(
        &mut items,
        "i.title",
        "CASE
           WHEN EXISTS (
             SELECT 1 FROM item_roles ir
             WHERE ir.item_id = i.id AND ir.role = 'review_requested'
           ) THEN 'review_requested'
           WHEN EXISTS (
             SELECT 1 FROM item_roles ir
             WHERE ir.item_id = i.id AND ir.role = 'assigned'
           ) THEN 'assign'
           WHEN EXISTS (
             SELECT 1 FROM item_roles ir
             WHERE ir.item_id = i.id AND ir.role = 'mentioned'
           ) THEN 'mention'
           ELSE 'subscribed'
         END",
        "r.owner",
        "r.name",
        "i.number",
        search,
        &mut args,
    );

    (format!("{notifs} UNION ALL {items}"), args)
}

fn apply_inbox_repo_filter(
    sql: &mut String,
    owner_col: &str,
    repo_col: &str,
    allowed_repos: &[String],
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    if allowed_repos.is_empty() {
        return;
    }
    sql.push_str(&format!(" AND ({owner_col} || '/' || {repo_col}) IN ("));
    for (i, repo) in allowed_repos.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push('?');
        args.push(Box::new(repo.clone()));
    }
    sql.push(')');
}

fn apply_inbox_search(
    sql: &mut String,
    title_col: &str,
    reason_col: &str,
    owner_col: &str,
    repo_col: &str,
    number_col: &str,
    search: &str,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    let needle = search.trim();
    if needle.is_empty() {
        return;
    }
    sql.push_str(&format!(
        " AND ({title_col} LIKE ? OR {reason_col} LIKE ? OR ({owner_col} || '/' || {repo_col}) LIKE ? OR CAST({number_col} AS TEXT) LIKE ?)"
    ));
    let pat = format!("%{needle}%");
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat));
}

fn apply_view_filter(sql: &mut String, view: View) {
    match view {
        View::Inbox => sql.push_str(" AND 0"),
        View::AllPrs => {
            sql.push_str(
                " AND i.kind = 'pr' AND i.state = 'open' AND i.id IN (
                    SELECT item_id FROM item_roles
                    WHERE role IN ('authored', 'review_requested', 'reviewed', 'assigned')
                  )",
            );
        }
        View::MyPrs => {
            sql.push_str(
                " AND i.kind = 'pr' AND i.state = 'open' AND i.id IN (
                    SELECT item_id FROM item_roles WHERE role = 'authored'
                  )",
            );
        }
        View::ClosedPrs => {
            sql.push_str(" AND i.kind = 'pr' AND i.state IN ('closed', 'merged')");
        }
        View::AllIssues => {
            sql.push_str(" AND i.kind = 'issue' AND i.state = 'open'");
        }
        View::ClosedIssues => {
            sql.push_str(" AND i.kind = 'issue' AND i.state = 'closed'");
        }
    }
}

fn apply_state_filter(sql: &mut String, q: &ItemQuery) {
    if !q.view.uses_state_filter() {
        return;
    }
    match q.state {
        StateFilter::Open => sql.push_str(" AND i.state = 'open'"),
        StateFilter::Closed => sql.push_str(" AND i.state IN ('closed', 'merged')"),
        StateFilter::All => {}
    }
}

fn apply_time_filter(
    sql: &mut String,
    q: &ItemQuery,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    if !q.view.uses_time_filter() {
        return;
    }
    if let Some(cut) = q.time.cutoff() {
        sql.push_str(" AND i.updated_at >= ?");
        args.push(Box::new(
            cut.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ));
    }
}

fn apply_repo_filter(
    sql: &mut String,
    q: &ItemQuery,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    if q.allowed_repos.is_empty() {
        return;
    }
    sql.push_str(" AND (r.owner || '/' || r.name) IN (");
    for (i, repo) in q.allowed_repos.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push('?');
        args.push(Box::new(repo.clone()));
    }
    sql.push(')');
}

fn apply_search_filter(
    sql: &mut String,
    q: &ItemQuery,
    args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    let needle = q.search.trim();
    if needle.is_empty() {
        return;
    }
    sql.push_str(
        " AND (i.title LIKE ? OR (r.owner || '/' || r.name) LIKE ? OR CAST(i.number AS TEXT) LIKE ? OR IFNULL(i.author,'') LIKE ?)",
    );
    let pat = format!("%{needle}%");
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat.clone()));
    args.push(Box::new(pat));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HydratedItem, ItemQuery, Kind};

    fn sample(owner: &str, repo: &str, n: i64, kind: Kind, state: ItemState) -> HydratedItem {
        HydratedItem {
            node_id: Some(format!("N{owner}{repo}{n}")),
            owner: owner.into(),
            repo: repo.into(),
            number: n,
            kind,
            title: format!("title {n}"),
            body: "fixes #1".into(),
            state,
            author: Some("me".into()),
            draft: false,
            html_url: format!("https://github.com/{owner}/{repo}/issues/{n}"),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-08-01T00:00:00Z".into()),
            closed_at: None,
            merged_at: None,
            comments_count: 0,
            review_decision: None,
            additions: Some(10),
            deletions: Some(2),
            changed_files: Some(1),
            assignees: vec!["me".into()],
            labels: vec![Label {
                name: "bug".into(),
                color: "ff0000".into(),
            }],
            review_requests: vec![],
            reviews: vec![],
            links: crate::refs::extract_refs("fixes #1", owner, repo),
        }
    }

    #[test]
    fn lists_newest_updated_first() {
        let db = Db::open(":memory:").unwrap();
        let mut older = sample("acme", "box", 1, Kind::Pr, ItemState::Open);
        older.updated_at = Some("2026-01-01T00:00:00Z".into());
        let mut newer = sample("acme", "box", 2, Kind::Pr, ItemState::Open);
        newer.updated_at = Some("2026-08-01T00:00:00Z".into());
        let mut roles = BTreeSet::new();
        roles.insert(Role::Authored);
        db.upsert_item(&older, &roles, true, true).unwrap();
        db.upsert_item(&newer, &roles, false, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllPrs;
        q.time = crate::model::TimeRange::All;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].number, 2);
        assert_eq!(rows[1].number, 1);
    }

    #[test]
    fn upsert_and_inbox() {
        let db = Db::open(":memory:").unwrap();
        let item = sample("acme", "box", 12, Kind::Pr, ItemState::Open);
        let mut roles = BTreeSet::new();
        roles.insert(Role::Authored);
        roles.insert(Role::Assigned);
        db.upsert_item(&item, &roles, true, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllPrs;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary_role(), Role::Authored);
        assert!(rows[0].unread);

        let detail = db.get_detail(rows[0].id).unwrap().unwrap();
        assert_eq!(detail.labels.len(), 1);
        assert!(detail.links.iter().any(|l| l.number == 1));

        let stats = db.stats().unwrap();
        assert_eq!(stats.prs, 1);
        assert_eq!(stats.unread, 1);
        assert!(
            rows[0]
                .nested_links()
                .any(|l| l.number == 1 && l.kind == LinkKind::Closes),
            "PR should nest the issue it closes"
        );
    }

    #[test]
    fn review_summary_counts_requests() {
        let db = Db::open(":memory:").unwrap();
        let mut item = sample("acme", "box", 3, Kind::Pr, ItemState::Open);
        item.reviews = vec![Review {
            github_id: Some(1),
            author: "alice".into(),
            state: "APPROVED".into(),
            submitted_at: None,
            body: String::new(),
        }];
        item.review_requests = vec!["bob".into(), "carol".into()];
        item.review_decision = Some("REVIEW_REQUIRED".into());
        let mut roles = BTreeSet::new();
        roles.insert(Role::Authored);
        db.upsert_item(&item, &roles, false, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllPrs;
        q.state = StateFilter::Open;
        q.time = crate::model::TimeRange::All;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].approvals, 1);
        assert_eq!(rows[0].review_total, 3);
        assert_eq!(rows[0].review_summary(), "1/3");
    }

    #[test]
    fn issue_nests_linked_pr() {
        let db = Db::open(":memory:").unwrap();
        let issue = sample("acme", "box", 1, Kind::Issue, ItemState::Open);
        db.upsert_item(&issue, &BTreeSet::new(), false, true)
            .unwrap();
        let pr = sample("acme", "box", 12, Kind::Pr, ItemState::Open);
        let mut roles = BTreeSet::new();
        roles.insert(Role::Authored);
        db.upsert_item(&pr, &roles, false, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllIssues;
        q.time = crate::model::TimeRange::All;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Issue);
        assert!(
            rows[0]
                .nested_links()
                .any(|l| l.number == 12 && l.kind == LinkKind::Closes),
            "issue should nest the PR that closes it"
        );
    }

    #[test]
    fn rehomes_item_when_github_transfers_node() {
        let db = Db::open(":memory:").unwrap();
        let mut old = sample("acme", "old", 10, Kind::Issue, ItemState::Open);
        old.node_id = Some("ISSUE_abc".into());
        old.title = "before move".into();
        let mut roles = BTreeSet::new();
        roles.insert(Role::Authored);
        db.upsert_item(&old, &roles, false, true).unwrap();

        let mut moved = sample("acme", "new", 20, Kind::Issue, ItemState::Open);
        moved.node_id = Some("ISSUE_abc".into());
        moved.title = "after move".into();
        db.upsert_item(&moved, &roles, false, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllIssues;
        q.time = crate::model::TimeRange::All;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "new");
        assert_eq!(rows[0].number, 20);
        assert_eq!(rows[0].title, "after move");
        assert_eq!(rows[0].primary_role(), Role::Authored);
    }

    #[test]
    fn merges_duplicate_rows_for_same_node() {
        let db = Db::open(":memory:").unwrap();
        let mut old = sample("acme", "old", 10, Kind::Pr, ItemState::Open);
        old.node_id = Some("PR_abc".into());
        let mut authored = BTreeSet::new();
        authored.insert(Role::Authored);
        db.upsert_item(&old, &authored, false, true).unwrap();

        let mut at_dest = sample("acme", "new", 20, Kind::Pr, ItemState::Open);
        at_dest.node_id = Some("PR_other".into());
        let mut requested = BTreeSet::new();
        requested.insert(Role::ReviewRequested);
        db.upsert_item(&at_dest, &requested, false, true).unwrap();

        let mut incoming = sample("acme", "new", 20, Kind::Pr, ItemState::Open);
        incoming.node_id = Some("PR_abc".into());
        incoming.title = "canonical".into();
        db.upsert_item(&incoming, &requested, false, true).unwrap();

        let mut q = ItemQuery::default();
        q.view = View::AllPrs;
        q.time = crate::model::TimeRange::All;
        let rows = db.list(&q).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].repo, "new");
        assert_eq!(rows[0].number, 20);
        assert_eq!(rows[0].title, "canonical");
        assert!(rows[0].roles.contains(&Role::ReviewRequested));
    }

    #[test]
    fn inbox_includes_review_requests_and_skips_duplicates() {
        let db = Db::open(":memory:").unwrap();
        let mine = sample("acme", "box", 1, Kind::Pr, ItemState::Open);
        let mut authored = BTreeSet::new();
        authored.insert(Role::Authored);
        authored.insert(Role::Assigned);
        db.upsert_item(&mine, &authored, false, true).unwrap();

        let theirs = sample("acme", "box", 2, Kind::Pr, ItemState::Open);
        let mut requested = BTreeSet::new();
        requested.insert(Role::ReviewRequested);
        db.upsert_item(&theirs, &requested, false, true).unwrap();

        let rows = db.list_notifications("", &[]).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].number, Some(2));
        assert_eq!(rows[0].reason, "review_requested");
        assert_eq!(db.count_notifications("", &[]).unwrap(), 1);

        db.upsert_notification(
            "gh-2",
            true,
            "review_requested",
            "2026-08-02T00:00:00Z",
            "PullRequest",
            "title 2",
            "acme",
            "box",
            Some(2),
        )
        .unwrap();
        let rows = db.list_notifications("", &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].github_id, "gh-2");
        assert!(rows[0].unread);
    }
}
