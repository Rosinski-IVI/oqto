CREATE TABLE gitlab_connections (
    account_id TEXT PRIMARY KEY NOT NULL,
    host TEXT NOT NULL,
    path_prefix TEXT NOT NULL DEFAULT '',
    credential_id TEXT NOT NULL DEFAULT 'gitlab',
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision BETWEEN 1 AND 4294967295),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    validated_credential_revision TEXT,
    gitlab_username TEXT,
    validated_at TEXT,
    event_categories_json TEXT NOT NULL DEFAULT '["assigned_issues","authored_issues","mentions_review_requests"]',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE gitlab_project_mappings (
    account_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('remote', 'override')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (account_id, workspace_path),
    UNIQUE (account_id, project_path),
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);

CREATE TABLE gitlab_poll_cursors (
    account_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    category TEXT NOT NULL,
    cursor TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (account_id, workspace_path, project_path, category),
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);

CREATE TABLE gitlab_inbox_events (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    event_key TEXT NOT NULL,
    category TEXT NOT NULL,
    categories_json TEXT NOT NULL DEFAULT '[]',
    coalesce_key TEXT,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('issue', 'merge_request')),
    entity_iid INTEGER NOT NULL CHECK (entity_iid > 0),
    related_issue_iid INTEGER,
    discussion_id TEXT,
    session_id TEXT,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    url TEXT,
    occurred_at TEXT NOT NULL,
    read_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (account_id, event_key),
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);
CREATE INDEX gitlab_inbox_owner_created ON gitlab_inbox_events(account_id, created_at DESC);
CREATE UNIQUE INDEX gitlab_inbox_one_unread_entity
    ON gitlab_inbox_events(account_id, workspace_path, coalesce_key)
    WHERE coalesce_key IS NOT NULL AND read_at IS NULL;

CREATE TABLE gitlab_workflows (
    account_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    issue_iid INTEGER NOT NULL CHECK (issue_iid > 0),
    session_id TEXT,
    implementation_started INTEGER NOT NULL DEFAULT 0 CHECK (implementation_started IN (0, 1)),
    merge_request_iid INTEGER,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (account_id, workspace_path, issue_iid),
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);

CREATE TABLE gitlab_session_selections (
    account_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    issue_iid INTEGER NOT NULL CHECK (issue_iid > 0),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (account_id, session_id),
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);

CREATE TABLE gitlab_proposals (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    project_path TEXT NOT NULL,
    issue_iid INTEGER NOT NULL CHECK (issue_iid > 0),
    session_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('start_implementation', 'update_issue', 'comment', 'inline_review', 'discussion_reply')),
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'applied', 'failed', 'expired')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT,
    FOREIGN KEY (account_id) REFERENCES gitlab_connections(account_id) ON DELETE CASCADE
);
CREATE INDEX gitlab_proposals_owner ON gitlab_proposals(account_id, workspace_path, status, created_at DESC);
