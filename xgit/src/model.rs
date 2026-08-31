use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Pr,
    Issue,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Issue => "issue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pr" | "PullRequest" => Some(Self::Pr),
            "issue" | "Issue" => Some(Self::Issue),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pr => write!(f, "PR"),
            Self::Issue => write!(f, "Issue"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Closed,
    Merged,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "merged" => Self::Merged,
            "closed" => Self::Closed,
            _ => Self::Open,
        }
    }
}

impl fmt::Display for ItemState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str().to_ascii_uppercase())
    }
}

/// How the authenticated user is involved with an item.
///
/// Multiple roles are stored. The list shows the highest-priority one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Authored,
    Assigned,
    Reviewed,
    ReviewRequested,
    Commented,
    Mentioned,
    Involved,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Assigned => "assigned",
            Self::Reviewed => "reviewed",
            Self::ReviewRequested => "review_requested",
            Self::Commented => "commented",
            Self::Mentioned => "mentioned",
            Self::Involved => "involved",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Authored => "AUTHORED",
            Self::Assigned => "ASSIGNED",
            Self::Reviewed => "REVIEWED",
            Self::ReviewRequested => "REVIEW REQ",
            Self::Commented => "COMMENTED",
            Self::Mentioned => "MENTIONED",
            Self::Involved => "INVOLVED",
        }
    }

    /// Lower is more important.
    pub fn priority(self) -> u8 {
        match self {
            Self::Authored => 1,
            Self::Assigned => 2,
            Self::Reviewed => 3,
            Self::ReviewRequested => 4,
            Self::Commented => 5,
            Self::Mentioned => 6,
            Self::Involved => 7,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "authored" => Some(Self::Authored),
            "assigned" => Some(Self::Assigned),
            "reviewed" => Some(Self::Reviewed),
            "review_requested" => Some(Self::ReviewRequested),
            "commented" => Some(Self::Commented),
            "mentioned" => Some(Self::Mentioned),
            "involved" => Some(Self::Involved),
            _ => None,
        }
    }

    pub fn best(roles: &BTreeSet<Role>) -> Role {
        roles
            .iter()
            .copied()
            .min_by_key(|r| r.priority())
            .unwrap_or(Self::Involved)
    }

    /// Map a GitHub notification `reason` onto a role, if it has one.
    pub fn from_notification_reason(reason: &str) -> Option<Self> {
        match reason {
            "assign" => Some(Self::Assigned),
            "author" => Some(Self::Authored),
            "comment" => Some(Self::Commented),
            "mention" | "team_mention" => Some(Self::Mentioned),
            "review_requested" | "approval_requested" => Some(Self::ReviewRequested),
            "state_change" | "subscribed" | "manual" | "ci_activity" => Some(Self::Involved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Closes,
    Mentioned,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closes => "closes",
            Self::Mentioned => "mentioned",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "closes" => Self::Closes,
            _ => Self::Mentioned,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Label {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone)]
pub struct Review {
    pub github_id: Option<i64>,
    pub author: String,
    pub state: String,
    pub submitted_at: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub github_id: Option<i64>,
    pub kind: String,
    pub author: String,
    pub body: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IssueLink {
    pub repo: String,
    pub number: i64,
    pub kind: LinkKind,
    pub title: Option<String>,
    pub state: Option<ItemState>,
    pub to_id: Option<i64>,
}

/// Fully hydrated GitHub item ready to persist.
#[derive(Debug, Clone)]
pub struct HydratedItem {
    pub node_id: Option<String>,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub state: ItemState,
    pub author: Option<String>,
    pub draft: bool,
    pub html_url: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub comments_count: i64,
    pub review_decision: Option<String>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub changed_files: Option<i64>,
    pub assignees: Vec<String>,
    pub labels: Vec<Label>,
    pub review_requests: Vec<String>,
    pub reviews: Vec<Review>,
    pub links: Vec<IssueLink>,
}

impl HydratedItem {
    pub fn key(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    /// Roles we can prove from the object itself (authoritative on a full hydrate).
    pub fn field_roles(&self, me: &str) -> BTreeSet<Role> {
        let mut roles = BTreeSet::new();
        if self.author.as_deref().is_some_and(|a| eq_user(a, me)) {
            roles.insert(Role::Authored);
        }
        if self.assignees.iter().any(|a| eq_user(a, me)) {
            roles.insert(Role::Assigned);
        }
        if self.kind == Kind::Pr {
            if self
                .reviews
                .iter()
                .any(|r| eq_user(&r.author, me) && !r.state.eq_ignore_ascii_case("PENDING"))
            {
                roles.insert(Role::Reviewed);
            }
            if self.review_requests.iter().any(|r| eq_user(r, me)) {
                roles.insert(Role::ReviewRequested);
            }
        }
        roles
    }
}

pub fn eq_user(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Compact row used by list views.
#[derive(Debug, Clone)]
pub struct ItemRow {
    pub id: i64,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub kind: Kind,
    pub title: String,
    pub state: ItemState,
    pub author: Option<String>,
    pub draft: bool,
    pub html_url: Option<String>,
    pub updated_at: Option<String>,
    pub review_decision: Option<String>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub unread: bool,
    pub roles: BTreeSet<Role>,
    pub approvals: u32,
    pub review_total: u32,
    pub links: Vec<IssueLink>,
}

impl ItemRow {
    pub fn primary_role(&self) -> Role {
        Role::best(&self.roles)
    }

    pub fn repo_num(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    /// Approvals over people requested or already reviewing, e.g. `1/3`.
    pub fn review_summary(&self) -> String {
        if self.kind != Kind::Pr || self.review_total == 0 {
            return String::new();
        }
        format!("{}/{}", self.approvals, self.review_total)
    }

    pub fn shows_nested(&self, link: &IssueLink) -> bool {
        let self_key = format!("{}/{}", self.owner, self.repo);
        if link.number == self.number && link.repo.eq_ignore_ascii_case(&self_key) {
            return false;
        }
        link.kind == LinkKind::Closes || link.title.is_some()
    }

    /// Issues this PR closes or clearly mentions — shown nested in lists.
    pub fn nested_links(&self) -> impl Iterator<Item = &IssueLink> {
        self.links.iter().filter(|link| self.shows_nested(link))
    }

    pub fn linked_count(&self) -> usize {
        self.nested_links().count()
    }
}

/// Unique requested/reviewing people vs how many have approved.
pub fn review_progress(reviews: &[Review], requested: &[String]) -> (u32, u32) {
    let mut slots = BTreeSet::new();
    let mut approved = 0u32;
    for review in reviews {
        let key = review.author.to_ascii_lowercase();
        match review.state.to_ascii_uppercase().as_str() {
            "APPROVED" => {
                approved += 1;
                slots.insert(key);
            }
            "CHANGES_REQUESTED" | "PENDING" => {
                slots.insert(key);
            }
            _ => {}
        }
    }
    for login in requested {
        let key = login.to_ascii_lowercase();
        if !key.is_empty() {
            slots.insert(key);
        }
    }
    (approved, slots.len() as u32)
}

/// One GitHub notification thread, shown in Inbox.
#[derive(Debug, Clone)]
pub struct InboxRow {
    pub github_id: String,
    pub unread: bool,
    pub reason: String,
    pub updated_at: Option<String>,
    pub subject_type: String,
    pub owner: String,
    pub repo: String,
    pub number: Option<i64>,
    pub title: String,
    pub item_id: Option<i64>,
}

impl InboxRow {
    pub fn reason_label(&self) -> &str {
        match self.reason.as_str() {
            "review_requested" => "review",
            "approval_requested" => "approve",
            "mention" => "mention",
            "team_mention" => "team",
            "assign" => "assign",
            "author" => "author",
            "comment" => "comment",
            "state_change" => "state",
            "subscribed" => "watch",
            "ci_activity" => "ci",
            "security_alert" => "security",
            "manual" => "manual",
            other => other,
        }
    }

    pub fn type_label(&self) -> &str {
        match self.subject_type.as_str() {
            "PullRequest" => "PR",
            "Issue" => "Issue",
            "CheckSuite" | "CheckRun" => "Check",
            "Release" => "Release",
            "Discussion" => "Disc",
            "RepositoryDependabotAlertsThread" | "RepositoryVulnerabilityAlert" => "Alert",
            other => other,
        }
    }

    pub fn html_url(&self) -> Option<String> {
        let n = self.number?;
        let kind = if self.subject_type == "PullRequest" {
            "pull"
        } else {
            "issues"
        };
        Some(format!(
            "https://github.com/{}/{}/{kind}/{n}",
            self.owner, self.repo
        ))
    }
}

/// Full item + related rows for the detail pane.
#[derive(Debug, Clone)]
pub struct ItemDetail {
    pub row: ItemRow,
    pub body: String,
    pub created_at: Option<String>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub comments_count: i64,
    pub changed_files: Option<i64>,
    pub labels: Vec<Label>,
    pub assignees: Vec<String>,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub links: Vec<IssueLink>,
    pub comments_fetched_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Inbox,
    AllPrs,
    MyPrs,
    ClosedPrs,
    AllIssues,
    ClosedIssues,
}

impl View {
    pub const ALL: [View; 6] = [
        View::Inbox,
        View::AllPrs,
        View::MyPrs,
        View::ClosedPrs,
        View::AllIssues,
        View::ClosedIssues,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::AllPrs => "All PRs",
            Self::MyPrs => "My PRs",
            Self::ClosedPrs => "Closed PRs",
            Self::AllIssues => "All Issues",
            Self::ClosedIssues => "Closed Issues",
        }
    }

    pub fn shift(self, delta: i32) -> Self {
        let len = Self::ALL.len() as i32;
        let idx = Self::ALL.iter().position(|&v| v == self).unwrap_or(0) as i32;
        let next = (idx + delta).rem_euclid(len) as usize;
        Self::ALL[next]
    }

    pub fn uses_time_filter(self) -> bool {
        matches!(self, Self::ClosedPrs | Self::ClosedIssues)
    }

    pub fn uses_state_filter(self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    Open,
    Closed,
    All,
}

impl StateFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "any state",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Open => Self::All,
            Self::All => Self::Closed,
            Self::Closed => Self::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Hours(u32),
    Days(u32),
    Weeks(u32),
    Months(u32),
    Years(u32),
    All,
}

impl TimeRange {
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let input = input.trim();
        if input.eq_ignore_ascii_case("all") || input == "0" {
            return Ok(Self::All);
        }
        if input.len() < 2 {
            anyhow::bail!("invalid time range {input:?} (use 1h, 2d, 3w, 4m, 1y, all)");
        }
        let (num, unit) = input.split_at(input.len() - 1);
        let n: u32 = num
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid time range number in {input:?}"))?;
        if n == 0 {
            anyhow::bail!("time range must be a positive integer");
        }
        Ok(match unit {
            "h" | "H" => Self::Hours(n),
            "d" | "D" => Self::Days(n),
            "w" | "W" => Self::Weeks(n),
            "m" | "M" => Self::Months(n),
            "y" | "Y" => Self::Years(n),
            _ => anyhow::bail!("invalid time unit {unit:?} (use h/d/w/m/y)"),
        })
    }

    pub fn cutoff(self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        let delta = match self {
            Self::Hours(n) => chrono::Duration::hours(i64::from(n)),
            Self::Days(n) => chrono::Duration::days(i64::from(n)),
            Self::Weeks(n) => chrono::Duration::weeks(i64::from(n)),
            Self::Months(n) => chrono::Duration::days(i64::from(n) * 30),
            Self::Years(n) => chrono::Duration::days(i64::from(n) * 365),
            Self::All => return None,
        };
        Some(now - delta)
    }

    pub fn label(self) -> String {
        match self {
            Self::Hours(n) => format!("{n}h"),
            Self::Days(n) => format!("{n}d"),
            Self::Weeks(n) => format!("{n}w"),
            Self::Months(n) => format!("{n}m"),
            Self::Years(n) => format!("{n}y"),
            Self::All => "all".into(),
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Days(1) => Self::Days(3),
            Self::Days(3) => Self::Days(7),
            Self::Days(7) => Self::Days(30),
            Self::Days(30) => Self::Days(90),
            Self::Days(90) => Self::Years(1),
            Self::Years(1) => Self::All,
            Self::All => Self::Days(1),
            _ => Self::Days(1),
        }
    }

    pub fn github_since_date(self) -> Option<String> {
        self.cutoff().map(|t| t.format("%Y-%m-%d").to_string())
    }
}

impl Default for TimeRange {
    fn default() -> Self {
        Self::Days(30)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ItemQuery {
    pub view: View,
    pub time: TimeRange,
    pub state: StateFilter,
    pub search: String,
    pub allowed_repos: Vec<String>,
}

impl Default for View {
    fn default() -> Self {
        Self::Inbox
    }
}

impl Default for StateFilter {
    fn default() -> Self {
        Self::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_progress_approved_of_requested() {
        let reviews = vec![Review {
            github_id: None,
            author: "alice".into(),
            state: "APPROVED".into(),
            submitted_at: None,
            body: String::new(),
        }];
        let requested = vec!["bob".into(), "carol".into()];
        assert_eq!(review_progress(&reviews, &requested), (1, 3));
    }

    #[test]
    fn review_progress_does_not_double_count_approver() {
        let reviews = vec![Review {
            github_id: None,
            author: "alice".into(),
            state: "APPROVED".into(),
            submitted_at: None,
            body: String::new(),
        }];
        let requested = vec!["alice".into(), "bob".into()];
        assert_eq!(review_progress(&reviews, &requested), (1, 2));
    }

    #[test]
    fn role_priority_authored_beats_reviewed() {
        let mut roles = BTreeSet::new();
        roles.insert(Role::Reviewed);
        roles.insert(Role::Authored);
        roles.insert(Role::Mentioned);
        assert_eq!(Role::best(&roles), Role::Authored);
    }

    #[test]
    fn time_range_parse_and_cycle() {
        assert!(matches!(
            TimeRange::parse("3h").unwrap(),
            TimeRange::Hours(3)
        ));
        assert!(matches!(
            TimeRange::parse("2d").unwrap(),
            TimeRange::Days(2)
        ));
        assert!(matches!(TimeRange::parse("all").unwrap(), TimeRange::All));
        assert!(TimeRange::parse("x").is_err());
        assert_eq!(TimeRange::Days(30).cycle().label(), "90d");
        assert_eq!(TimeRange::All.cycle().label(), "1d");
    }

    #[test]
    fn view_shift_wraps() {
        assert_eq!(View::Inbox.shift(1), View::AllPrs);
        assert_eq!(View::Inbox.shift(-1), View::ClosedIssues);
        assert_eq!(View::ClosedIssues.shift(1), View::Inbox);
        assert_eq!(View::MyPrs.shift(2), View::AllIssues);
    }

    #[test]
    fn notification_reason_maps() {
        assert_eq!(
            Role::from_notification_reason("review_requested"),
            Some(Role::ReviewRequested)
        );
        assert_eq!(
            Role::from_notification_reason("mention"),
            Some(Role::Mentioned)
        );
        assert_eq!(
            Role::from_notification_reason("assign"),
            Some(Role::Assigned)
        );
    }
}
