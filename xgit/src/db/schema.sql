PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS repos (
    id    INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    name  TEXT NOT NULL,
    UNIQUE (owner, name)
);

CREATE TABLE IF NOT EXISTS items (
    id                 INTEGER PRIMARY KEY,
    repo_id            INTEGER NOT NULL REFERENCES repos(id),
    number             INTEGER NOT NULL,
    node_id            TEXT UNIQUE,
    kind               TEXT NOT NULL CHECK (kind IN ('pr', 'issue')),
    title              TEXT NOT NULL,
    body               TEXT,
    state              TEXT NOT NULL CHECK (state IN ('open', 'closed', 'merged')),
    author             TEXT,
    draft              INTEGER NOT NULL DEFAULT 0,
    html_url           TEXT,
    created_at         TEXT,
    updated_at         TEXT,
    closed_at          TEXT,
    merged_at          TEXT,
    comments_count     INTEGER DEFAULT 0,
    review_decision    TEXT,
    additions          INTEGER,
    deletions          INTEGER,
    changed_files      INTEGER,
    unread             INTEGER NOT NULL DEFAULT 0,
    last_hydrated_at   TEXT,
    comments_fetched_at TEXT,
    UNIQUE (repo_id, number)
);

CREATE TABLE IF NOT EXISTS item_roles (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    role    TEXT NOT NULL,
    PRIMARY KEY (item_id, role)
);

CREATE TABLE IF NOT EXISTS item_labels (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    name    TEXT NOT NULL,
    color   TEXT,
    PRIMARY KEY (item_id, name)
);

CREATE TABLE IF NOT EXISTS item_assignees (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    login   TEXT NOT NULL,
    PRIMARY KEY (item_id, login)
);

CREATE TABLE IF NOT EXISTS item_review_requests (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    login   TEXT NOT NULL,
    PRIMARY KEY (item_id, login)
);

CREATE TABLE IF NOT EXISTS reviews (
    id           INTEGER PRIMARY KEY,
    item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    github_id    INTEGER,
    author       TEXT,
    state        TEXT,
    submitted_at TEXT,
    body         TEXT
);

CREATE TABLE IF NOT EXISTS comments (
    id        INTEGER PRIMARY KEY,
    item_id   INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    github_id INTEGER,
    kind      TEXT NOT NULL,
    author    TEXT,
    body      TEXT,
    created_at TEXT,
    UNIQUE (item_id, kind, github_id)
);

CREATE TABLE IF NOT EXISTS links (
    from_id   INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    to_repo   TEXT NOT NULL,
    to_number INTEGER NOT NULL,
    to_id     INTEGER REFERENCES items(id) ON DELETE SET NULL,
    kind      TEXT NOT NULL,
    PRIMARY KEY (from_id, to_repo, to_number, kind)
);

CREATE INDEX IF NOT EXISTS idx_items_updated ON items(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_items_state   ON items(state);
CREATE INDEX IF NOT EXISTS idx_items_kind    ON items(kind);
CREATE INDEX IF NOT EXISTS idx_items_unread  ON items(unread);
CREATE INDEX IF NOT EXISTS idx_roles_role    ON item_roles(role);
CREATE INDEX IF NOT EXISTS idx_items_repo    ON items(repo_id, number);

CREATE TABLE IF NOT EXISTS notifications (
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
