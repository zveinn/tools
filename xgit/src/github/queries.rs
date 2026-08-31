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
