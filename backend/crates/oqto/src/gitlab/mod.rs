//! Account-owned GitLab connection metadata, project mapping, inbox and
//! proposal persistence. Credential bytes remain in the runner's secret vault.
use anyhow::{Context, Result, ensure};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

pub const TOKEN_CREDENTIAL_ID: &str = "gitlab";
pub const EVENT_CATEGORIES: [&str; 4] = [
    "assigned_issues",
    "authored_issues",
    "mentions_review_requests",
    "project_activity",
];

/// Accept HTTPS GitLab on the standard TLS port with a bounded safe path prefix.
/// Private DNS names are allowed; IP literals, plaintext HTTP, custom ports,
/// userinfo, encoded segments, dot segments, and unsafe path characters are not.
pub fn normalize_instance_url(input: &str) -> Result<(String, String)> {
    let (_, after_scheme) = input
        .split_once("://")
        .context("GitLab URL must include a scheme")?;
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let raw_suffix = &after_scheme[authority_end..];
    let raw_authority = &after_scheme[..authority_end];
    ensure!(
        !raw_authority.contains('@'),
        "GitLab URL credentials are not allowed"
    );
    let raw_path = raw_suffix
        .split(['?', '#'])
        .next()
        .filter(|path| !path.is_empty())
        .unwrap_or("/");
    ensure!(
        !raw_path.contains('%'),
        "encoded GitLab URL path segments are unsupported"
    );
    normalize_path_prefix(raw_path)?;
    let url = Url::parse(input).context("GitLab URL must be an HTTPS origin")?;
    ensure!(url.scheme() == "https", "GitLab requires HTTPS");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "GitLab URL credentials are not allowed"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "GitLab URL query/fragment is not allowed"
    );
    ensure!(
        url.port_or_known_default() == Some(443),
        "GitLab must use TLS port 443"
    );
    let host = url
        .host_str()
        .context("GitLab host is required")?
        .to_ascii_lowercase();
    validate_instance_host(&host)?;
    let path_prefix = normalize_path_prefix(url.path())?;
    Ok((host, path_prefix))
}

pub fn normalize_path_prefix(path: &str) -> Result<String> {
    if path.is_empty() || path == "/" {
        return Ok(String::new());
    }
    ensure!(
        path.starts_with('/') && !path.contains("//") && path.len() <= 1024,
        "invalid GitLab URL path prefix"
    );
    let trimmed = path.trim_end_matches('/');
    ensure!(!trimmed.is_empty(), "invalid GitLab URL path prefix");
    ensure!(
        trimmed.split('/').skip(1).all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
                })
        }),
        "invalid GitLab URL path prefix"
    );
    Ok(trimmed.to_owned())
}

fn validate_instance_host(host: &str) -> Result<()> {
    ensure!(
        host.len() <= 253 && host.contains('.') && host == host.to_ascii_lowercase(),
        "invalid GitLab DNS host"
    );
    ensure!(
        host.parse::<std::net::IpAddr>().is_err(),
        "GitLab IP literals are unsupported"
    );
    ensure!(
        host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        }),
        "invalid GitLab DNS host"
    );
    Ok(())
}

pub fn validate_project_path(project: &str) -> Result<()> {
    ensure!(
        !project.is_empty()
            && project.len() <= 512
            && !project.starts_with('/')
            && !project.ends_with('/'),
        "invalid GitLab project path"
    );
    ensure!(
        project.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part.len() <= 128
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        }),
        "invalid GitLab project path"
    );
    Ok(())
}

/// Map an HTTPS/SSH Git remote to a configured GitLab project. Credentials in
/// a remote URL are ignored and never persisted. The remote host must match
/// the one configured instance exactly.
pub fn project_from_remote(
    remote: &str,
    configured_host: &str,
    path_prefix: &str,
) -> Result<String> {
    validate_instance_host(configured_host)?;
    normalize_path_prefix(path_prefix)?;
    let (host, raw_path, ssh_remote) = if let Some(rest) = remote.strip_prefix("git@") {
        let (host, path) = rest
            .split_once(':')
            .context("invalid scp-style Git remote")?;
        (host.to_owned(), path.to_owned(), true)
    } else {
        let (_, after_scheme) = remote.split_once("://").context("invalid Git remote URL")?;
        let authority_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let suffix = &after_scheme[authority_end..];
        let raw_path = suffix.split(['?', '#']).next().unwrap_or_default();
        ensure!(
            !raw_path.contains('%') && !raw_path.contains("//"),
            "encoded or repeated Git remote path segments are unsupported"
        );
        for segment in raw_path
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
        {
            ensure!(
                !matches!(segment, "." | ".."),
                "Git remote path traversal is unsupported"
            );
        }
        let url = Url::parse(remote).context("invalid Git remote URL")?;
        ensure!(
            matches!(url.scheme(), "https" | "ssh"),
            "unsupported Git remote scheme"
        );
        ensure!(
            url.query().is_none() && url.fragment().is_none(),
            "Git remote query/fragment is unsupported"
        );
        let host = url.host_str().context("Git remote host is required")?;
        let ssh_remote = url.scheme() == "ssh";
        if url.scheme() == "https" {
            ensure!(
                url.port_or_known_default() == Some(443),
                "Git remote must use standard HTTPS port"
            );
        }
        (
            host.to_owned(),
            url.path()
                .strip_prefix('/')
                .context("invalid Git remote path")?
                .to_owned(),
            ssh_remote,
        )
    };
    ensure!(
        host.eq_ignore_ascii_case(configured_host),
        "Git remote does not match configured GitLab host"
    );
    let prefix = path_prefix.trim_start_matches('/');
    let relative_path = if prefix.is_empty() {
        raw_path.as_str()
    } else {
        match raw_path
            .strip_prefix(prefix)
            .and_then(|path| path.strip_prefix('/'))
        {
            Some(path) => path,
            None if ssh_remote => raw_path.as_str(),
            None => anyhow::bail!("Git remote does not match the configured GitLab path prefix"),
        }
    };
    let project = relative_path.strip_suffix(".git").unwrap_or(relative_path);
    validate_project_path(project)?;
    Ok(project.to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitLabConnection {
    pub host: String,
    pub path_prefix: String,
    pub credential_id: String,
    pub revision: u32,
    pub enabled: bool,
    pub validated_credential_revision: Option<String>,
    pub gitlab_username: Option<String>,
    pub validated_at: Option<String>,
    pub event_categories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectMapping {
    pub workspace_path: String,
    pub project_path: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEvent {
    pub id: String,
    pub workspace_path: String,
    pub project_path: String,
    pub category: String,
    pub categories: Vec<String>,
    pub entity_kind: String,
    pub entity_iid: i64,
    pub related_issue_iid: Option<i64>,
    pub discussion_id: Option<String>,
    pub session_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub url: Option<String>,
    pub occurred_at: String,
    pub read_at: Option<String>,
    #[serde(skip)]
    pub coalesce_key: Option<String>,
}

#[derive(Debug, FromRow)]
struct InboxEventRow {
    id: String,
    workspace_path: String,
    project_path: String,
    category: String,
    categories_json: String,
    entity_kind: String,
    entity_iid: i64,
    related_issue_iid: Option<i64>,
    discussion_id: Option<String>,
    session_id: Option<String>,
    title: String,
    summary: String,
    url: Option<String>,
    occurred_at: String,
    read_at: Option<String>,
}

impl TryFrom<InboxEventRow> for InboxEvent {
    type Error = anyhow::Error;

    fn try_from(row: InboxEventRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            workspace_path: row.workspace_path,
            project_path: row.project_path,
            category: row.category,
            categories: serde_json::from_str(&row.categories_json)
                .context("decode GitLab event categories")?,
            entity_kind: row.entity_kind,
            entity_iid: row.entity_iid,
            related_issue_iid: row.related_issue_iid,
            discussion_id: row.discussion_id,
            session_id: row.session_id,
            title: row.title,
            summary: row.summary,
            url: row.url,
            occurred_at: row.occurred_at,
            read_at: row.read_at,
            coalesce_key: None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub workspace_path: String,
    pub project_path: String,
    pub issue_iid: u64,
    pub session_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub session_stale: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct TrackedMergeRequest {
    pub project_path: String,
    pub issue_iid: i64,
    pub merge_request_iid: i64,
    pub session_id: Option<String>,
}

#[derive(Debug, FromRow)]
struct ConnectionRow {
    host: String,
    path_prefix: String,
    credential_id: String,
    revision: i64,
    enabled: bool,
    validated_credential_revision: Option<String>,
    gitlab_username: Option<String>,
    validated_at: Option<String>,
    event_categories_json: String,
}

#[derive(Debug, Clone)]
pub struct GitLabRepository {
    pool: SqlitePool,
}

fn merge_categories(existing_json: &str, incoming: &[String]) -> Result<Vec<String>> {
    let mut categories: Vec<String> =
        serde_json::from_str(existing_json).context("decode persisted GitLab event categories")?;
    for category in incoming {
        if !categories.iter().any(|current| current == category) {
            categories.push(category.clone());
        }
    }
    categories.sort();
    Ok(categories)
}

impl GitLabRepository {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn connection(&self, account_id: &str) -> Result<Option<GitLabConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT host,path_prefix,credential_id,revision,enabled,validated_credential_revision,gitlab_username,validated_at,event_categories_json FROM gitlab_connections WHERE account_id=?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .context("load GitLab connection")?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(GitLabConnection {
            host: row.host,
            path_prefix: row.path_prefix,
            credential_id: row.credential_id,
            revision: u32::try_from(row.revision).context("GitLab revision overflow")?,
            enabled: row.enabled,
            validated_credential_revision: row.validated_credential_revision,
            gitlab_username: row.gitlab_username,
            validated_at: row.validated_at,
            event_categories: serde_json::from_str(&row.event_categories_json)
                .context("decode GitLab event categories")?,
        }))
    }

    pub async fn save_connection(
        &self,
        account_id: &str,
        host: &str,
        path_prefix: &str,
        enabled: bool,
        categories: &[String],
    ) -> Result<GitLabConnection> {
        let categories_json = serde_json::to_string(categories)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab connection update")?;
        let previous = sqlx::query_as::<_, (String, String)>(
            "SELECT host,path_prefix FROM gitlab_connections WHERE account_id=?",
        )
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("read previous GitLab instance")?;
        if previous
            .as_ref()
            .is_some_and(|(previous_host, previous_prefix)| {
                previous_host != host || previous_prefix != path_prefix
            })
        {
            for table in [
                "gitlab_project_mappings",
                "gitlab_poll_cursors",
                "gitlab_inbox_events",
                "gitlab_workflows",
                "gitlab_session_selections",
                "gitlab_proposals",
            ] {
                let query = format!("DELETE FROM {table} WHERE account_id=?");
                sqlx::query(&query)
                    .bind(account_id)
                    .execute(&mut *transaction)
                    .await
                    .context("clear stale GitLab instance state")?;
            }
        }
        sqlx::query(
            "INSERT INTO gitlab_connections(account_id,host,path_prefix,credential_id,revision,enabled,event_categories_json) VALUES(?,?,?, ?,1,?,?) ON CONFLICT(account_id) DO UPDATE SET validated_credential_revision=CASE WHEN gitlab_connections.host<>excluded.host OR gitlab_connections.path_prefix<>excluded.path_prefix THEN NULL ELSE gitlab_connections.validated_credential_revision END,gitlab_username=CASE WHEN gitlab_connections.host<>excluded.host OR gitlab_connections.path_prefix<>excluded.path_prefix THEN NULL ELSE gitlab_connections.gitlab_username END,validated_at=CASE WHEN gitlab_connections.host<>excluded.host OR gitlab_connections.path_prefix<>excluded.path_prefix THEN NULL ELSE gitlab_connections.validated_at END,host=excluded.host,path_prefix=excluded.path_prefix,credential_id=excluded.credential_id,revision=gitlab_connections.revision+1,enabled=excluded.enabled,event_categories_json=excluded.event_categories_json,updated_at=datetime('now')",
        )
        .bind(account_id)
        .bind(host)
        .bind(path_prefix)
        .bind(TOKEN_CREDENTIAL_ID)
        .bind(enabled)
        .bind(categories_json)
        .execute(&mut *transaction)
        .await
        .context("save GitLab connection")?;
        transaction
            .commit()
            .await
            .context("commit GitLab connection update")?;
        self.connection(account_id)
            .await?
            .context("saved GitLab connection missing")
    }

    pub async fn delete_connection(&self, account_id: &str) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM gitlab_connections WHERE account_id=?")
                .bind(account_id)
                .execute(&self.pool)
                .await
                .context("delete GitLab connection")?
                .rows_affected()
                == 1,
        )
    }

    pub async fn mark_connection_tested(
        &self,
        account_id: &str,
        host: &str,
        path_prefix: &str,
        connection_revision: u32,
        credential_revision: &str,
        username: &str,
    ) -> Result<bool> {
        let validated_at = chrono::Utc::now().to_rfc3339();
        Ok(sqlx::query("UPDATE gitlab_connections SET validated_credential_revision=?,gitlab_username=?,validated_at=? WHERE account_id=? AND host=? AND path_prefix=? AND revision=? AND enabled=1")
            .bind(credential_revision)
            .bind(username)
            .bind(validated_at)
            .bind(account_id)
            .bind(host)
            .bind(path_prefix)
            .bind(connection_revision)
            .execute(&self.pool)
            .await
            .context("record tested GitLab credential revision")?
            .rows_affected() == 1)
    }

    pub async fn mapping(
        &self,
        account_id: &str,
        workspace_path: &str,
    ) -> Result<Option<ProjectMapping>> {
        #[derive(FromRow)]
        struct Row {
            workspace_path: String,
            project_path: String,
            source: String,
        }
        let row = sqlx::query_as::<_, Row>(
            "SELECT workspace_path,project_path,source FROM gitlab_project_mappings WHERE account_id=? AND workspace_path=?",
        )
        .bind(account_id)
        .bind(workspace_path)
        .fetch_optional(&self.pool)
        .await
        .context("load GitLab project mapping")?;
        Ok(row.map(|row| ProjectMapping {
            workspace_path: row.workspace_path,
            project_path: row.project_path,
            source: row.source,
        }))
    }

    pub async fn save_mapping(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        source: &str,
    ) -> Result<ProjectMapping> {
        ensure!(
            matches!(source, "remote" | "override"),
            "invalid mapping source"
        );
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab project mapping update")?;
        let previous_project = sqlx::query_scalar::<_, String>(
            "SELECT project_path FROM gitlab_project_mappings WHERE account_id=? AND workspace_path=?",
        )
        .bind(account_id)
        .bind(workspace_path)
        .fetch_optional(&mut *transaction)
        .await
        .context("read previous GitLab project mapping")?;
        if previous_project
            .as_deref()
            .is_some_and(|previous| previous != project_path)
        {
            for table in [
                "gitlab_poll_cursors",
                "gitlab_inbox_events",
                "gitlab_workflows",
                "gitlab_session_selections",
                "gitlab_proposals",
            ] {
                let query = format!("DELETE FROM {table} WHERE account_id=? AND workspace_path=?");
                sqlx::query(&query)
                    .bind(account_id)
                    .bind(workspace_path)
                    .execute(&mut *transaction)
                    .await
                    .context("clear stale GitLab project state")?;
            }
        }
        sqlx::query(
            "INSERT INTO gitlab_project_mappings(account_id,workspace_path,project_path,source) VALUES(?,?,?,?) ON CONFLICT(account_id,workspace_path) DO UPDATE SET project_path=excluded.project_path,source=excluded.source,updated_at=datetime('now')",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .bind(source)
        .execute(&mut *transaction)
        .await
        .context("save GitLab project mapping")?;
        transaction
            .commit()
            .await
            .context("commit GitLab project mapping")?;
        Ok(ProjectMapping {
            workspace_path: workspace_path.to_owned(),
            project_path: project_path.to_owned(),
            source: source.to_owned(),
        })
    }

    pub async fn project_mapped_elsewhere(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
    ) -> Result<bool> {
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT workspace_path FROM gitlab_project_mappings WHERE account_id=? AND project_path=? AND workspace_path<>?",
        )
        .bind(account_id)
        .bind(project_path)
        .bind(workspace_path)
        .fetch_optional(&self.pool)
        .await
        .context("check duplicate GitLab project mapping")?;
        Ok(owner.is_some())
    }

    pub async fn delete_mapping(&self, account_id: &str, workspace_path: &str) -> Result<bool> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab mapping removal")?;
        for table in [
            "gitlab_poll_cursors",
            "gitlab_inbox_events",
            "gitlab_workflows",
            "gitlab_session_selections",
            "gitlab_proposals",
        ] {
            let query = format!("DELETE FROM {table} WHERE account_id=? AND workspace_path=?");
            sqlx::query(&query)
                .bind(account_id)
                .bind(workspace_path)
                .execute(&mut *transaction)
                .await
                .context("clear removed GitLab mapping state")?;
        }
        let deleted = sqlx::query(
            "DELETE FROM gitlab_project_mappings WHERE account_id=? AND workspace_path=?",
        )
        .bind(account_id)
        .bind(workspace_path)
        .execute(&mut *transaction)
        .await
        .context("remove GitLab project mapping")?
        .rows_affected()
            == 1;
        transaction
            .commit()
            .await
            .context("commit GitLab mapping removal")?;
        Ok(deleted)
    }

    pub async fn cursor(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        category: &str,
    ) -> Result<Option<String>> {
        let value = sqlx::query_scalar::<_, Option<String>>(
            "SELECT cursor FROM gitlab_poll_cursors WHERE account_id=? AND workspace_path=? AND project_path=? AND category=?",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .bind(category)
        .fetch_optional(&self.pool)
        .await
        .context("load GitLab polling cursor")?;
        Ok(value.flatten())
    }

    pub async fn save_cursor(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        category: &str,
        cursor: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO gitlab_poll_cursors(account_id,workspace_path,project_path,category,cursor) VALUES(?,?,?,?,?) ON CONFLICT(account_id,workspace_path,project_path,category) DO UPDATE SET cursor=excluded.cursor,updated_at=datetime('now')",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .bind(category)
        .bind(cursor)
        .execute(&self.pool)
        .await
        .context("save GitLab polling cursor")?;
        Ok(())
    }

    pub async fn record_event(
        &self,
        account_id: &str,
        event: &InboxEvent,
        event_key: &str,
    ) -> Result<bool> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab inbox update")?;
        let exact = sqlx::query_as::<_, (String, String)>(
            "SELECT id,categories_json FROM gitlab_inbox_events WHERE account_id=? AND event_key=?",
        )
        .bind(account_id)
        .bind(event_key)
        .fetch_optional(&mut *transaction)
        .await
        .context("find duplicate GitLab event")?;
        if let Some((id, categories_json)) = exact {
            let categories = merge_categories(&categories_json, &event.categories)?;
            sqlx::query("UPDATE gitlab_inbox_events SET categories_json=?,discussion_id=COALESCE(discussion_id,?),related_issue_iid=COALESCE(related_issue_iid,?),session_id=COALESCE(session_id,?) WHERE account_id=? AND id=?")
                .bind(serde_json::to_string(&categories)?)
                .bind(&event.discussion_id)
                .bind(event.related_issue_iid)
                .bind(&event.session_id)
                .bind(account_id)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .context("merge GitLab event categories")?;
            transaction
                .commit()
                .await
                .context("commit GitLab event category merge")?;
            return Ok(false);
        }

        if let Some(coalesce_key) = event.coalesce_key.as_deref() {
            let existing = sqlx::query_as::<_, (String, String, String)>(
                "SELECT id,event_key,categories_json FROM gitlab_inbox_events WHERE account_id=? AND workspace_path=? AND coalesce_key=? AND read_at IS NULL",
            )
            .bind(account_id)
            .bind(&event.workspace_path)
            .bind(coalesce_key)
            .fetch_optional(&mut *transaction)
            .await
            .context("find unread GitLab event to coalesce")?;
            if let Some((id, previous_key, categories_json)) = existing {
                let categories = if previous_key == event_key {
                    merge_categories(&categories_json, &event.categories)?
                } else {
                    event.categories.clone()
                };
                sqlx::query("UPDATE gitlab_inbox_events SET event_key=?,category=?,categories_json=?,entity_kind=?,entity_iid=?,related_issue_iid=?,discussion_id=?,session_id=?,title=?,summary=?,url=?,occurred_at=? WHERE account_id=? AND id=? AND read_at IS NULL")
                    .bind(event_key)
                    .bind(categories.first().map(String::as_str).unwrap_or(&event.category))
                    .bind(serde_json::to_string(&categories)?)
                    .bind(&event.entity_kind)
                    .bind(event.entity_iid)
                    .bind(event.related_issue_iid)
                    .bind(&event.discussion_id)
                    .bind(&event.session_id)
                    .bind(&event.title)
                    .bind(&event.summary)
                    .bind(&event.url)
                    .bind(&event.occurred_at)
                    .bind(account_id)
                    .bind(id)
                    .execute(&mut *transaction)
                    .await
                    .context("coalesce unread GitLab event")?;
                transaction
                    .commit()
                    .await
                    .context("commit GitLab event coalesce")?;
                return Ok(false);
            }
        }

        sqlx::query(
            "INSERT OR IGNORE INTO gitlab_inbox_events(id,account_id,workspace_path,project_path,event_key,category,categories_json,coalesce_key,entity_kind,entity_iid,related_issue_iid,discussion_id,session_id,title,summary,url,occurred_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&event.id)
        .bind(account_id)
        .bind(&event.workspace_path)
        .bind(&event.project_path)
        .bind(event_key)
        .bind(&event.category)
        .bind(serde_json::to_string(&event.categories)?)
        .bind(&event.coalesce_key)
        .bind(&event.entity_kind)
        .bind(event.entity_iid)
        .bind(event.related_issue_iid)
        .bind(&event.discussion_id)
        .bind(&event.session_id)
        .bind(&event.title)
        .bind(&event.summary)
        .bind(&event.url)
        .bind(&event.occurred_at)
        .execute(&mut *transaction)
        .await
        .context("record GitLab inbox event")?;
        transaction
            .commit()
            .await
            .context("commit GitLab inbox event")?;
        Ok(true)
    }

    pub async fn expire_and_prune(&self) -> Result<(u64, u64)> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab retention cleanup")?;
        let expired = sqlx::query("UPDATE gitlab_proposals SET status='expired',decided_at=datetime('now') WHERE status='pending' AND created_at<=datetime('now','-30 days')")
            .execute(&mut *transaction)
            .await
            .context("expire old GitLab proposals")?
            .rows_affected();
        let removed = sqlx::query(
            "DELETE FROM gitlab_inbox_events WHERE created_at<=datetime('now','-90 days')",
        )
        .execute(&mut *transaction)
        .await
        .context("prune old GitLab inbox events")?
        .rows_affected();
        transaction
            .commit()
            .await
            .context("commit GitLab retention cleanup")?;
        Ok((expired, removed))
    }

    pub async fn update_proposal_payload(
        &self,
        account_id: &str,
        id: &str,
        payload: &serde_json::Value,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE gitlab_proposals SET payload_json=? WHERE account_id=? AND id=? AND status='approved'")
            .bind(serde_json::to_string(payload)?)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("update GitLab proposal progress")?
            .rows_affected() == 1)
    }

    pub async fn update_pending_proposal_payload(
        &self,
        account_id: &str,
        id: &str,
        payload: &serde_json::Value,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE gitlab_proposals SET payload_json=? WHERE account_id=? AND id=? AND status='pending'")
            .bind(serde_json::to_string(payload)?)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("update pending GitLab proposal snapshot")?
            .rows_affected() == 1)
    }

    pub async fn rebind_proposal(
        &self,
        account_id: &str,
        id: &str,
        session_id: &str,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE gitlab_proposals SET session_id=?,status='pending',decided_at=NULL WHERE account_id=? AND id=? AND status IN ('pending','approved')")
            .bind(session_id)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("rebind GitLab proposal to session")?
            .rows_affected() == 1)
    }

    pub async fn inbox(&self, account_id: &str, workspace_path: &str) -> Result<Vec<InboxEvent>> {
        let rows = sqlx::query_as::<_, InboxEventRow>(
            "SELECT id,workspace_path,project_path,category,categories_json,entity_kind,entity_iid,related_issue_iid,discussion_id,session_id,title,summary,url,occurred_at,read_at FROM gitlab_inbox_events WHERE account_id=? AND workspace_path=? ORDER BY occurred_at DESC,id DESC LIMIT 200",
        )
        .bind(account_id)
        .bind(workspace_path)
        .fetch_all(&self.pool)
        .await
        .context("list GitLab inbox events")?;
        rows.into_iter().map(InboxEvent::try_from).collect()
    }

    pub async fn mark_event_read(
        &self,
        account_id: &str,
        event_id: &str,
    ) -> Result<Option<String>> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin GitLab read update")?;
        let workspace_path = sqlx::query_scalar::<_, String>(
            "SELECT workspace_path FROM gitlab_inbox_events WHERE account_id=? AND id=?",
        )
        .bind(account_id)
        .bind(event_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("find owned GitLab event")?;
        let Some(workspace_path) = workspace_path else {
            transaction
                .rollback()
                .await
                .context("rollback missing GitLab event read")?;
            return Ok(None);
        };
        sqlx::query("UPDATE gitlab_inbox_events SET read_at=COALESCE(read_at,datetime('now')) WHERE account_id=? AND id=?")
            .bind(account_id)
            .bind(event_id)
            .execute(&mut *transaction)
            .await
            .context("mark GitLab event read")?;
        transaction
            .commit()
            .await
            .context("commit GitLab read update")?;
        Ok(Some(workspace_path))
    }

    pub async fn unread_counts(&self, account_id: &str) -> Result<(i64, Vec<(String, i64)>)> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gitlab_inbox_events WHERE account_id=? AND read_at IS NULL",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .context("count unread GitLab events")?;
        let workspaces = sqlx::query_as::<_, (String, i64)>(
            "SELECT workspace_path,COUNT(*) FROM gitlab_inbox_events WHERE account_id=? AND read_at IS NULL GROUP BY workspace_path ORDER BY workspace_path",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .context("count unread GitLab events by workspace")?;
        Ok((total, workspaces))
    }

    pub async fn poll_targets(&self) -> Result<Vec<(String, String)>> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT m.account_id,m.workspace_path FROM gitlab_project_mappings m JOIN gitlab_connections c ON c.account_id=m.account_id WHERE c.enabled=1 ORDER BY m.account_id,m.workspace_path",
        )
        .fetch_all(&self.pool)
        .await
        .context("list GitLab poll targets")
    }

    pub async fn create_proposal(&self, account_id: &str, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            "INSERT INTO gitlab_proposals(id,account_id,workspace_path,project_path,issue_iid,session_id,kind,payload_json,status) VALUES(?,?,?,?,?,?,?,?,'pending')",
        )
        .bind(&proposal.id)
        .bind(account_id)
        .bind(&proposal.workspace_path)
        .bind(&proposal.project_path)
        .bind(i64::try_from(proposal.issue_iid).context("GitLab issue IID overflow")?)
        .bind(&proposal.session_id)
        .bind(&proposal.kind)
        .bind(serde_json::to_string(&proposal.payload)?)
        .execute(&self.pool)
        .await
        .context("create GitLab proposal")?;
        Ok(())
    }

    pub async fn proposals(&self, account_id: &str, workspace_path: &str) -> Result<Vec<Proposal>> {
        #[derive(FromRow)]
        struct Row {
            id: String,
            workspace_path: String,
            project_path: String,
            issue_iid: i64,
            session_id: Option<String>,
            kind: String,
            payload_json: String,
            status: String,
            created_at: String,
        }
        let rows = sqlx::query_as::<_, Row>(
            "SELECT id,workspace_path,project_path,issue_iid,session_id,kind,payload_json,status,created_at FROM gitlab_proposals WHERE account_id=? AND workspace_path=? ORDER BY created_at DESC LIMIT 100",
        )
        .bind(account_id)
        .bind(workspace_path)
        .fetch_all(&self.pool)
        .await
        .context("list GitLab proposals")?;
        rows.into_iter()
            .map(|row| {
                Ok(Proposal {
                    id: row.id,
                    workspace_path: row.workspace_path,
                    project_path: row.project_path,
                    issue_iid: u64::try_from(row.issue_iid).context("invalid GitLab issue id")?,
                    session_id: row.session_id,
                    kind: row.kind,
                    payload: serde_json::from_str(&row.payload_json)
                        .context("decode GitLab proposal")?,
                    status: row.status,
                    created_at: row.created_at,
                    session_stale: false,
                })
            })
            .collect()
    }

    pub async fn proposal(&self, account_id: &str, id: &str) -> Result<Option<Proposal>> {
        let rows = sqlx::query_as::<_, (String, String, String, i64, Option<String>, String, String, String, String)>(
            "SELECT id,workspace_path,project_path,issue_iid,session_id,kind,payload_json,status,created_at FROM gitlab_proposals WHERE account_id=? AND id=?",
        )
        .bind(account_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("load owned GitLab proposal")?;
        rows.map(|row| {
            Ok(Proposal {
                id: row.0,
                workspace_path: row.1,
                project_path: row.2,
                issue_iid: u64::try_from(row.3).context("invalid GitLab issue id")?,
                session_id: row.4,
                kind: row.5,
                payload: serde_json::from_str(&row.6).context("decode GitLab proposal")?,
                status: row.7,
                created_at: row.8,
                session_stale: false,
            })
        })
        .transpose()
    }

    pub async fn decide_proposal(&self, account_id: &str, id: &str, status: &str) -> Result<bool> {
        ensure!(
            matches!(status, "approved" | "rejected" | "applied" | "failed"),
            "invalid proposal state"
        );
        let expected = if matches!(status, "applied" | "failed") {
            "approved"
        } else {
            "pending"
        };
        Ok(sqlx::query("UPDATE gitlab_proposals SET status=?,decided_at=datetime('now') WHERE account_id=? AND id=? AND status=?")
            .bind(status).bind(account_id).bind(id).bind(expected).execute(&self.pool).await
            .context("decide GitLab proposal")?.rows_affected() == 1)
    }

    pub async fn start_implementation(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        issue_iid: u64,
        session_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO gitlab_workflows(account_id,workspace_path,project_path,issue_iid,session_id,implementation_started) VALUES(?,?,?,?,?,1) ON CONFLICT(account_id,workspace_path,issue_iid) DO UPDATE SET session_id=COALESCE(excluded.session_id,gitlab_workflows.session_id),implementation_started=1,updated_at=datetime('now')",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .bind(i64::try_from(issue_iid).context("GitLab issue IID overflow")?)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .context("record approved GitLab implementation start")?;
        Ok(())
    }

    pub async fn workflow_started(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        issue_iid: u64,
    ) -> Result<bool> {
        let value: Option<bool> = sqlx::query_scalar(
            "SELECT implementation_started FROM gitlab_workflows WHERE account_id=? AND workspace_path=? AND project_path=? AND issue_iid=?",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .bind(i64::try_from(issue_iid).context("GitLab issue IID overflow")?)
        .fetch_optional(&self.pool)
        .await
        .context("check GitLab implementation approval")?;
        Ok(value.unwrap_or(false))
    }

    pub async fn save_merge_request(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
        issue_iid: u64,
        merge_request_iid: u64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO gitlab_workflows(account_id,workspace_path,project_path,issue_iid,implementation_started,merge_request_iid) VALUES(?,?,?, ?,1,?) ON CONFLICT(account_id,workspace_path,issue_iid) DO UPDATE SET merge_request_iid=excluded.merge_request_iid,updated_at=datetime('now')")
            .bind(account_id).bind(workspace_path).bind(project_path).bind(i64::try_from(issue_iid).context("GitLab issue IID overflow")?).bind(i64::try_from(merge_request_iid).context("GitLab merge request IID overflow")?)
            .execute(&self.pool).await.context("save GitLab workflow merge request")?;
        Ok(())
    }

    pub async fn tracked_merge_requests(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
    ) -> Result<Vec<TrackedMergeRequest>> {
        sqlx::query_as::<_, TrackedMergeRequest>(
            "SELECT project_path,issue_iid,merge_request_iid,session_id FROM gitlab_workflows WHERE account_id=? AND workspace_path=? AND project_path=? AND implementation_started=1 AND merge_request_iid IS NOT NULL ORDER BY updated_at DESC",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .fetch_all(&self.pool)
        .await
            .context("list GitLab workflow merge requests")
    }

    pub async fn tracked_issue_merge_requests(
        &self,
        account_id: &str,
        workspace_path: &str,
        project_path: &str,
    ) -> Result<std::collections::HashMap<u64, u64>> {
        let rows = sqlx::query_as::<_, (i64, i64)>(
            "SELECT issue_iid,merge_request_iid FROM gitlab_workflows WHERE account_id=? AND workspace_path=? AND project_path=? AND implementation_started=1 AND merge_request_iid IS NOT NULL",
        )
        .bind(account_id)
        .bind(workspace_path)
        .bind(project_path)
        .fetch_all(&self.pool)
        .await
        .context("list issue-to-merge-request links")?;
        rows.into_iter()
            .map(|(issue, merge_request)| {
                Ok((
                    u64::try_from(issue).context("invalid stored issue IID")?,
                    u64::try_from(merge_request).context("invalid stored merge request IID")?,
                ))
            })
            .collect()
    }

    pub async fn select_issue_for_session(
        &self,
        account_id: &str,
        session_id: &str,
        workspace_path: &str,
        project_path: &str,
        issue_iid: u64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO gitlab_session_selections(account_id,session_id,workspace_path,project_path,issue_iid) VALUES(?,?,?,?,?) ON CONFLICT(account_id,session_id) DO UPDATE SET workspace_path=excluded.workspace_path,project_path=excluded.project_path,issue_iid=excluded.issue_iid,updated_at=datetime('now')")
            .bind(account_id)
            .bind(session_id)
            .bind(workspace_path)
            .bind(project_path)
            .bind(i64::try_from(issue_iid).context("GitLab issue IID overflow")?)
            .execute(&self.pool)
            .await
            .context("bind selected GitLab issue to session")?;
        Ok(())
    }

    pub async fn selected_issue_for_session(
        &self,
        account_id: &str,
        session_id: &str,
    ) -> Result<Option<(String, String, u64)>> {
        let row = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT workspace_path,project_path,issue_iid FROM gitlab_session_selections WHERE account_id=? AND session_id=?",
        )
        .bind(account_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .context("load selected GitLab issue for session")?;
        row.map(|(workspace, project, issue)| {
            Ok((
                workspace,
                project,
                u64::try_from(issue).context("invalid selected GitLab issue IID")?,
            ))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn allows_private_https_origins_but_rejects_unscoped_or_plaintext_urls() {
        assert_eq!(
            normalize_instance_url("https://gitlab.corp.internal").unwrap(),
            ("gitlab.corp.internal".to_owned(), String::new())
        );
        assert_eq!(
            normalize_instance_url("https://gitlab.corp.internal/gitlab/team/").unwrap(),
            ("gitlab.corp.internal".to_owned(), "/gitlab/team".to_owned())
        );
        assert!(normalize_instance_url("http://gitlab.example").is_err());
        assert!(normalize_instance_url("https://127.0.0.1").is_err());
        assert!(normalize_instance_url("https://gitlab.example:8443").is_err());
        assert!(normalize_instance_url("https://user:token@gitlab.example").is_err());
        for unsafe_path in [
            "https://gitlab.example/a/../b",
            "https://gitlab.example/a%2fb",
            "https://gitlab.example/a//b",
            "https://gitlab.example/a?query=1",
        ] {
            assert!(
                normalize_instance_url(unsafe_path).is_err(),
                "{unsafe_path}"
            );
        }
    }

    #[test]
    fn project_mapping_requires_the_configured_remote_and_strips_dot_git() {
        assert_eq!(
            project_from_remote(
                "https://user:secret@gitlab.example/team/sub/project.git",
                "gitlab.example",
                "",
            )
            .unwrap(),
            "team/sub/project"
        );
        assert_eq!(
            project_from_remote("git@gitlab.example:team/project.git", "gitlab.example", "")
                .unwrap(),
            "team/project"
        );
        assert_eq!(
            project_from_remote(
                "https://gitlab.example/gitlab/team/project.git",
                "gitlab.example",
                "/gitlab",
            )
            .unwrap(),
            "team/project"
        );
        assert_eq!(
            project_from_remote(
                "git@gitlab.example:team/project.git",
                "gitlab.example",
                "/gitlab"
            )
            .unwrap(),
            "team/project"
        );
        assert!(
            project_from_remote(
                "https://evil.example/team/project.git",
                "gitlab.example",
                ""
            )
            .is_err()
        );
        assert!(
            project_from_remote(
                "https://gitlab.example/team/../secrets.git",
                "gitlab.example",
                ""
            )
            .is_err()
        );
        assert!(
            project_from_remote(
                "http://gitlab.example/team/project.git",
                "gitlab.example",
                ""
            )
            .is_err()
        );
        assert!(
            project_from_remote(
                "https://gitlab.example/team/project.git",
                "gitlab.example",
                "/gitlab"
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn mappings_cursors_events_and_proposals_are_workspace_scoped_and_deduplicated()
    -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/20261104001_gitlab_agent_workflow.sql"
        ))
        .execute(&pool)
        .await?;
        let repository = GitLabRepository::new(pool);
        let connection = repository
            .save_connection(
                "alice",
                "gitlab.corp.internal",
                "",
                true,
                &["assigned_issues".into()],
            )
            .await?;
        assert_eq!(connection.credential_id, TOKEN_CREDENTIAL_ID);
        repository
            .save_mapping("alice", "/work/one", "team/one", "remote")
            .await?;
        repository
            .save_mapping("alice", "/work/two", "team/two", "override")
            .await?;
        repository
            .select_issue_for_session("alice", "session-1", "/work/one", "team/one", 5)
            .await?;
        assert_eq!(
            repository
                .selected_issue_for_session("alice", "session-1")
                .await?,
            Some(("/work/one".into(), "team/one".into(), 5))
        );
        assert!(
            repository
                .save_mapping("alice", "/work/duplicate", "team/one", "override")
                .await
                .is_err()
        );
        repository
            .save_cursor(
                "alice",
                "/work/one",
                "team/one",
                "assigned",
                "2026-11-04T10:00:00Z",
            )
            .await?;
        assert_eq!(
            repository
                .cursor("alice", "/work/one", "team/one", "assigned")
                .await?,
            Some("2026-11-04T10:00:00Z".to_owned())
        );
        assert_eq!(
            repository
                .cursor("alice", "/work/two", "team/one", "assigned")
                .await?,
            None
        );

        let event = InboxEvent {
            id: "event-1".into(),
            workspace_path: "/work/one".into(),
            project_path: "team/one".into(),
            category: "assigned_issues".into(),
            categories: vec!["assigned_issues".into()],
            coalesce_key: Some("/work/one:team/one:issue:5".into()),
            entity_kind: "issue".into(),
            entity_iid: 5,
            related_issue_iid: None,
            discussion_id: None,
            session_id: None,
            title: "Issue".into(),
            summary: "Summary".into(),
            url: None,
            occurred_at: "2026-11-04T10:00:00Z".into(),
            read_at: None,
        };
        repository
            .record_event("alice", &event, "issue:5:update")
            .await?;
        let duplicate = InboxEvent {
            id: "event-2".into(),
            category: "authored_issues".into(),
            categories: vec!["authored_issues".into()],
            ..event.clone()
        };
        repository
            .record_event("alice", &duplicate, "issue:5:update")
            .await?;
        let enriched = InboxEvent {
            id: "event-3".into(),
            category: "mentions_review_requests".into(),
            categories: vec!["mentions_review_requests".into()],
            discussion_id: Some("discussion-1".into()),
            ..event.clone()
        };
        repository
            .record_event("alice", &enriched, "issue:5:update")
            .await?;
        let inbox = repository.inbox("alice", "/work/one").await?;
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].categories,
            vec![
                "assigned_issues".to_owned(),
                "authored_issues".to_owned(),
                "mentions_review_requests".to_owned(),
            ]
        );
        assert_eq!(inbox[0].discussion_id.as_deref(), Some("discussion-1"));
        let updated = InboxEvent {
            id: "event-4".into(),
            title: "Updated issue".into(),
            occurred_at: "2026-11-04T10:01:00Z".into(),
            ..event.clone()
        };
        repository
            .record_event("alice", &updated, "issue:5:update-2")
            .await?;
        let inbox = repository.inbox("alice", "/work/one").await?;
        assert_eq!(inbox.len(), 1, "unread updates coalesce");
        assert_eq!(inbox[0].title, "Updated issue");
        assert_eq!(
            repository.mark_event_read("alice", "event-1").await?,
            Some("/work/one".to_owned())
        );
        let read_again = InboxEvent {
            id: "event-4".into(),
            title: "Another update".into(),
            occurred_at: "2026-11-04T10:02:00Z".into(),
            ..event.clone()
        };
        repository
            .record_event("alice", &read_again, "issue:5:update-3")
            .await?;
        assert_eq!(repository.inbox("alice", "/work/one").await?.len(), 2);
        assert_eq!(repository.unread_counts("alice").await?.0, 1);

        let proposal = Proposal {
            id: "proposal-1".into(),
            workspace_path: "/work/one".into(),
            project_path: "team/one".into(),
            issue_iid: 5,
            session_id: Some("session-1".into()),
            kind: "comment".into(),
            payload: serde_json::json!({"body":"hello"}),
            status: "pending".into(),
            created_at: "2026-11-04T10:00:00Z".into(),
            session_stale: false,
        };
        repository.create_proposal("alice", &proposal).await?;
        assert!(
            repository
                .decide_proposal("alice", "proposal-1", "approved")
                .await?
        );
        assert!(
            !repository
                .decide_proposal("alice", "proposal-1", "approved")
                .await?
        );
        assert!(
            repository
                .decide_proposal("alice", "proposal-1", "applied")
                .await?
        );
        assert_eq!(
            repository.proposals("alice", "/work/one").await?[0].status,
            "applied"
        );
        let reply_proposal = Proposal {
            id: "proposal-reply".into(),
            kind: "discussion_reply".into(),
            payload: serde_json::json!({"discussion_id":"discussion-1","body":"reply"}),
            ..proposal.clone()
        };
        repository.create_proposal("alice", &reply_proposal).await?;
        assert!(
            repository
                .decide_proposal("alice", "proposal-reply", "rejected")
                .await?
        );
        let expired = Proposal {
            id: "proposal-expired".into(),
            status: "pending".into(),
            ..proposal.clone()
        };
        repository.create_proposal("alice", &expired).await?;
        sqlx::query("UPDATE gitlab_proposals SET created_at=datetime('now','-31 days') WHERE id='proposal-expired'")
            .execute(repository.pool())
            .await?;
        sqlx::query("UPDATE gitlab_inbox_events SET created_at=datetime('now','-91 days') WHERE account_id='alice'")
            .execute(repository.pool())
            .await?;
        assert_eq!(repository.expire_and_prune().await?, (1, 2));
        assert_eq!(
            repository
                .proposal("alice", "proposal-expired")
                .await?
                .unwrap()
                .status,
            "expired"
        );
        assert!(repository.inbox("alice", "/work/one").await?.is_empty());
        repository
            .save_connection("alice", "gitlab-new.corp.internal", "", true, &[])
            .await?;
        assert_eq!(
            repository
                .selected_issue_for_session("alice", "session-1")
                .await?,
            None
        );
        assert_eq!(repository.mapping("alice", "/work/one").await?, None);
        assert!(repository.proposals("alice", "/work/one").await?.is_empty());
        assert!(repository.inbox("alice", "/work/one").await?.is_empty());
        Ok(())
    }
}
