//! GraphQL documents. Fragments are shared so a search page, a node batch,
//! and an alias hydrate all return the same shape.

pub const ITEM_FRAGMENTS: &str = r#"
fragment ActorLogin on Actor { login }

fragment IssueFields on Issue {
  __typename
  id
  number
  title
  body
  state
  url
  createdAt
  updatedAt
  closedAt
  author { ...ActorLogin }
  comments(first: 20) {
    totalCount
    nodes { body }
  }
  assignees(first: 10) { nodes { login } }
  labels(first: 20) { nodes { name color } }
  closedByPullRequestsReferences(first: 15) {
    nodes {
      number
      title
      state
      url
      repository { nameWithOwner }
    }
  }
  timelineItems(first: 30, itemTypes: [CROSS_REFERENCED_EVENT, CONNECTED_EVENT]) {
    nodes {
      __typename
      ... on CrossReferencedEvent {
        willCloseTarget
        source {
          __typename
          ... on Issue {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
          ... on PullRequest {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
        }
      }
      ... on ConnectedEvent {
        subject {
          __typename
          ... on Issue {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
          ... on PullRequest {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
        }
      }
    }
  }
  repository { owner { login } name }
}

fragment PrFields on PullRequest {
  __typename
  id
  number
  title
  body
  state
  merged
  mergedAt
  isDraft
  url
  createdAt
  updatedAt
  closedAt
  author { ...ActorLogin }
  reviewDecision
  additions
  deletions
  changedFiles
  comments(first: 20) {
    totalCount
    nodes { body }
  }
  assignees(first: 10) { nodes { login } }
  labels(first: 20) { nodes { name color } }
  reviewRequests(first: 40) {
    nodes {
      requestedReviewer {
        __typename
        ... on User { login }
      }
    }
  }
  latestReviews(first: 40) {
    nodes {
      databaseId
      author { login }
      state
      submittedAt
      body
    }
  }
  closingIssuesReferences(first: 15) {
    nodes {
      number
      title
      state
      url
      repository { nameWithOwner }
    }
  }
  timelineItems(first: 30, itemTypes: [CROSS_REFERENCED_EVENT, CONNECTED_EVENT]) {
    nodes {
      __typename
      ... on CrossReferencedEvent {
        willCloseTarget
        source {
          __typename
          ... on Issue {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
          ... on PullRequest {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
        }
      }
      ... on ConnectedEvent {
        subject {
          __typename
          ... on Issue {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
          ... on PullRequest {
            number
            title
            state
            url
            repository { nameWithOwner }
          }
        }
      }
    }
  }
  repository { owner { login } name }
}

"#;

pub const ITEM_ON_UNION: &str = r#"
fragment Item on IssueOrPullRequest {
  ... on Issue { ...IssueFields }
  ... on PullRequest { ...PrFields }
}
"#;

/// Lean per-repo shape for the live repo browser: enough for the list rows
/// and the preview, without the timeline and comment-body walks that make
/// [`ITEM_FRAGMENTS`] too heavy to ask for 50 at a time (GitHub 502s).
pub const REPO_BROWSE: &str = r#"
fragment RepoPr on PullRequest {
  __typename
  id number title body state merged mergedAt isDraft url
  createdAt updatedAt closedAt
  additions deletions changedFiles reviewDecision
  author { login }
  comments { totalCount }
  assignees(first: 10) { nodes { login } }
  labels(first: 10) { nodes { name color } }
  reviewRequests(first: 20) {
    nodes { requestedReviewer { __typename ... on User { login } } }
  }
  latestReviews(first: 20) {
    nodes { databaseId author { login } state submittedAt body }
  }
  repository { owner { login } name }
}

fragment RepoIssue on Issue {
  __typename
  id number title body state url
  createdAt updatedAt closedAt
  author { login }
  comments { totalCount }
  assignees(first: 10) { nodes { login } }
  labels(first: 10) { nodes { name color } }
  repository { owner { login } name }
}

query RepoBrowse($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequests(
      first: $n
      states: OPEN
      orderBy: { field: UPDATED_AT, direction: DESC }
    ) {
      totalCount
      nodes { ...RepoPr }
    }
    issues(
      first: $n
      states: OPEN
      orderBy: { field: UPDATED_AT, direction: DESC }
    ) {
      totalCount
      nodes { ...RepoIssue }
    }
  }
  rateLimit { cost remaining resetAt limit }
}
"#;

pub const VIEWER: &str = r#"
query Viewer {
  viewer { login }
  rateLimit { cost remaining resetAt limit }
}
"#;

const SEARCH_BODY: &str = r#"
query Search($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 50, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      __typename
      ... on Issue { ...IssueFields }
      ... on PullRequest { ...PrFields }
    }
  }
  rateLimit { cost remaining resetAt limit }
}
"#;

const NODES_BODY: &str = r#"
query Nodes($ids: [ID!]!) {
  nodes(ids: $ids) {
    __typename
    ... on Issue { ...IssueFields }
    ... on PullRequest { ...PrFields }
  }
  rateLimit { cost remaining resetAt limit }
}
"#;

pub fn search() -> String {
    format!("{ITEM_FRAGMENTS}\n{SEARCH_BODY}")
}

pub fn nodes() -> String {
    format!("{ITEM_FRAGMENTS}\n{NODES_BODY}")
}

/// Repositories the viewer owns personally, with GitHub's own open counts.
/// `ownerAffiliations: [OWNER]` excludes org repos and repos they only
/// collaborate on.
///
/// PR authors come along so Dependabot can be split out of the open-PR count;
/// `totalCount` stays authoritative for the total. Pages are kept small
/// because each repo pulls up to `OWNED_REPO_PR_SAMPLE` PR nodes.
pub const OWNED_REPOS: &str = r#"
query OwnedRepos($after: String) {
  viewer {
    repositories(
      first: 25
      after: $after
      ownerAffiliations: [OWNER]
      orderBy: { field: PUSHED_AT, direction: DESC }
    ) {
      pageInfo { hasNextPage endCursor }
      nodes {
        name
        pushedAt
        owner { login }
        pullRequests(states: OPEN, first: 100) {
          totalCount
          nodes { author { login } }
        }
        issues(states: OPEN) { totalCount }
      }
    }
  }
  rateLimit { cost remaining resetAt limit }
}
"#;

pub const COMMENTS: &str = r#"
query Comments($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      __typename
      ... on Issue {
        comments(first: 60) {
          nodes { databaseId author { login } body createdAt }
        }
      }
      ... on PullRequest {
        comments(first: 40) {
          nodes { databaseId author { login } body createdAt }
        }
        latestReviews(first: 40) {
          nodes { databaseId author { login } body state submittedAt }
        }
      }
    }
  }
  rateLimit { cost remaining resetAt limit }
}
"#;
