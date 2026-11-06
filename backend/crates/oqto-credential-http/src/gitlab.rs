use super::{AuthorizedUse, Error, JsonResult, TrustedAuthority};
use oqto_protocol::gitlab::{
    GitLabDiscussionPosition, GitLabHttpDispatch, GitLabIssueScope, GitLabMergeRequestScope,
    GitLabIssueState, GitLabMergeRequestState, GitLabOperation,
};
use reqwest::{Method, Url, header::HeaderValue};
use serde_json::{Value, json};
use std::{net::{IpAddr, SocketAddr}, time::Duration};
use zeroize::Zeroizing;

const MAX_GITLAB_BODY: usize = 512 * 1024;
const GITLAB_DEADLINE: Duration = Duration::from_secs(15);

/// Bind trusted audit evidence to a validated GitLab host and connection
/// revision. The GitLab operation still cannot select a destination.
pub fn gitlab_authorized_use(
    host: &str,
    path_prefix: &str,
    revision: u64,
    evidence: super::UseEvidence,
) -> Result<AuthorizedUse, Error> {
    validate_host(host)?;
    validate_path_prefix(path_prefix)?;
    let api_prefix = format!("{path_prefix}/api/v4/");
    let policy = super::DestinationPolicy::new(
        host,
        &api_prefix,
        super::CredentialHeader::Authorization,
        super::Attachment::Raw,
        revision,
    )?;
    AuthorizedUse::new(policy, evidence)
}

/// Execute one closed-vocabulary GitLab REST operation. The caller must pass a
/// host from the account's persisted GitLab connection; no URL is accepted.
/// Private DNS addresses are supported, but unsafe local/link-local endpoints,
/// redirects, plaintext HTTP, invalid TLS, and token echoes are rejected.
pub async fn gitlab_json(
    authority: &impl TrustedAuthority,
    lease: AuthorizedUse,
    request: GitLabHttpDispatch,
) -> Result<JsonResult, Error> {
    validate_host(&request.host)?;
    validate_path_prefix(&request.path_prefix)?;
    if lease.policy.host != request.host
        || lease.policy.prefix != format!("{}/api/v4/", request.path_prefix)
        || lease.policy.revision != u64::from(request.connection_revision)
    {
        return Err(Error::Authority);
    }
    let (method, relative, body) = operation_request(request.operation)?;
    let url = Url::parse(&format!(
        "https://{}{}{}",
        request.host, request.path_prefix, relative
    ))
        .map_err(|_| Error::Input)?;
    let expected_api_prefix = format!("{}/api/v4/", request.path_prefix);
    if url.scheme() != "https"
        || url.host_str() != Some(request.host.as_str())
        || url.port_or_known_default() != Some(443)
        || !url.path().starts_with(&expected_api_prefix)
    {
        return Err(Error::Input);
    }
    if let Some(value) = &body {
        if serde_json::to_vec(value).map_err(|_| Error::Input)?.len() > MAX_GITLAB_BODY {
            return Err(Error::Input);
        }
    }

    tokio::time::timeout(GITLAB_DEADLINE, execute(authority, lease, method, url, body, request.host))
        .await
        .map_err(|_| Error::Deadline)?
}

fn validate_host(host: &str) -> Result<(), Error> {
    if host.len() > 253
        || !host.contains('.')
        || host != host.to_ascii_lowercase()
        || host.parse::<IpAddr>().is_ok()
        || host.ends_with('.')
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                })
        })
    {
        return Err(Error::Policy);
    }
    Ok(())
}

fn validate_path_prefix(prefix: &str) -> Result<(), Error> {
    if prefix.is_empty() {
        return Ok(());
    }
    if !prefix.starts_with('/')
        || prefix.ends_with('/')
        || prefix.contains("//")
        || prefix.len() > 1024
        || prefix.split('/').skip(1).any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || !part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
                })
        })
    {
        return Err(Error::Policy);
    }
    Ok(())
}

fn project_segment(project: &str) -> Result<String, Error> {
    if project.is_empty() || project.len() > 512 || project.starts_with('/') || project.ends_with('/') {
        return Err(Error::Input);
    }
    if project.split('/').any(|part| {
        part.is_empty()
            || matches!(part, "." | "..")
            || part.len() > 128
            || !part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
            })
    }) {
        return Err(Error::Input);
    }
    Ok(percent_encode(project.as_bytes()))
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut result = String::new();
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            result.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

fn safe_timestamp(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'T' | b't' | b'Z' | b'z' | b':' | b'.' | b'+' | b'-'))
    {
        return Err(Error::Input);
    }
    Ok(())
}

fn issue_scope(scope: GitLabIssueScope) -> &'static str {
    match scope {
        GitLabIssueScope::AssignedToMe => "assigned_to_me",
        GitLabIssueScope::CreatedByMe => "created_by_me",
        GitLabIssueScope::All => "all",
    }
}

fn issue_state(state: GitLabIssueState) -> &'static str {
    match state {
        GitLabIssueState::Opened => "opened",
        GitLabIssueState::Closed => "closed",
        GitLabIssueState::All => "all",
    }
}

fn mr_scope(scope: GitLabMergeRequestScope) -> &'static str {
    match scope {
        GitLabMergeRequestScope::AssignedToMe => "assigned_to_me",
        GitLabMergeRequestScope::All => "all",
    }
}

fn issue_body(
    title: Option<String>,
    description: Option<String>,
    labels: Option<Vec<String>>,
) -> Result<Value, Error> {
    if title.is_none() && description.is_none() && labels.is_none() {
        return Err(Error::Input);
    }
    let mut body = serde_json::Map::new();
    if let Some(title) = title {
        if title.trim().is_empty() || title.len() > 255 {
            return Err(Error::Input);
        }
        body.insert("title".into(), Value::String(title));
    }
    if let Some(description) = description {
        if description.len() > 64 * 1024 {
            return Err(Error::Input);
        }
        body.insert("description".into(), Value::String(description));
    }
    if let Some(labels) = labels {
        if labels.len() > 50 || labels.iter().any(|label| label.len() > 128) {
            return Err(Error::Input);
        }
        body.insert("labels".into(), Value::String(labels.join(",")));
    }
    Ok(Value::Object(body))
}

fn draft_title(title: String, draft: bool) -> Result<String, Error> {
    let plain = title.trim_start_matches("Draft: ").trim_start_matches("WIP: ");
    if plain.trim().is_empty() || plain.len() > 255 {
        return Err(Error::Input);
    }
    Ok(if draft { format!("Draft: {plain}") } else { plain.to_owned() })
}

fn validate_discussion(position: &GitLabDiscussionPosition, body: &str) -> Result<(), Error> {
    let path_is_safe = |path: &str| {
        !path.is_empty()
            && path.len() <= 1024
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
    };
    if body.trim().is_empty()
        || body.len() > 32 * 1024
        || !path_is_safe(&position.old_path)
        || !path_is_safe(&position.new_path)
        || position.old_line.is_some() == position.new_line.is_some()
        || position.old_line.is_some_and(|line| line == 0 || line > u64::from(u32::MAX))
        || position.new_line.is_some_and(|line| line == 0 || line > u64::from(u32::MAX))
        || [&position.base_sha, &position.start_sha, &position.head_sha]
            .iter()
            .any(|sha| sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(Error::Input);
    }
    Ok(())
}

fn operation_request(operation: GitLabOperation) -> Result<(Method, String, Option<Value>), Error> {
    use GitLabOperation as Op;
    match operation {
        Op::CurrentUser => Ok((Method::GET, "/api/v4/user".into(), None)),
        Op::GetProject { project_path } => Ok((
            Method::GET,
            format!("/api/v4/projects/{}", project_segment(&project_path)?),
            None,
        )),
        Op::ListProjectLabels { project_path, page } => Ok((
            Method::GET,
            format!(
                "/api/v4/projects/{}/labels?per_page=50&page={}",
                project_segment(&project_path)?,
                page.max(1)
            ),
            None,
        )),
        Op::ListIssues { project_path, scope, state, updated_after, page } => {
            let project = project_segment(&project_path)?;
            let mut url = Url::parse(&format!("https://gitlab.invalid/api/v4/projects/{project}/issues"))
                .map_err(|_| Error::Input)?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("scope", issue_scope(scope));
                query.append_pair("state", issue_state(state));
                query.append_pair("per_page", "50");
                query.append_pair("page", &page.max(1).to_string());
                query.append_pair("order_by", "updated_at");
                query.append_pair("sort", "asc");
                if let Some(updated_after) = updated_after {
                    safe_timestamp(&updated_after)?;
                    query.append_pair("updated_after", &updated_after);
                }
            }
            let path = format!("{}?{}", url.path(), url.query().ok_or(Error::Input)?);
            Ok((Method::GET, path, None))
        }
        Op::GetIssue { project_path, issue_iid } => {
            if issue_iid == 0 { return Err(Error::Input); }
            Ok((Method::GET, format!("/api/v4/projects/{}/issues/{issue_iid}", project_segment(&project_path)?), None))
        }
        Op::ListIssueNotes { project_path, issue_iid, page } => {
            if issue_iid == 0 { return Err(Error::Input); }
            Ok((Method::GET, format!("/api/v4/projects/{}/issues/{issue_iid}/notes?per_page=50&page={}&order_by=created_at&sort=desc", project_segment(&project_path)?, page.max(1)), None))
        }
        Op::UpdateIssue { project_path, issue_iid, title, description, labels, state_event } => {
            if issue_iid == 0 { return Err(Error::Input); }
            let mut body = if title.is_none() && description.is_none() && labels.is_none() && state_event.is_some() {
                Value::Object(serde_json::Map::new())
            } else {
                issue_body(title, description, labels)?
            };
            if let Some(state_event) = state_event {
                let Value::Object(ref mut values) = body else { return Err(Error::Input); };
                values.insert(
                    "state_event".into(),
                    Value::String(match state_event {
                        oqto_protocol::gitlab::GitLabIssueStateEvent::Close => "close",
                        oqto_protocol::gitlab::GitLabIssueStateEvent::Reopen => "reopen",
                    }.to_owned()),
                );
            }
            Ok((Method::PUT, format!("/api/v4/projects/{}/issues/{issue_iid}", project_segment(&project_path)?), Some(body)))
        }
        Op::CreateIssueNote { project_path, issue_iid, body } => {
            if issue_iid == 0 || body.trim().is_empty() || body.len() > 32 * 1024 { return Err(Error::Input); }
            Ok((Method::POST, format!("/api/v4/projects/{}/issues/{issue_iid}/notes", project_segment(&project_path)?), Some(json!({"body": body}))))
        }
        Op::ListMergeRequests { project_path, scope, state, reviewer_id, updated_after, page } => {
            let project = project_segment(&project_path)?;
            let mut url = Url::parse(&format!("https://gitlab.invalid/api/v4/projects/{project}/merge_requests"))
                .map_err(|_| Error::Input)?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("scope", mr_scope(scope));
                query.append_pair("state", match state {
                    GitLabMergeRequestState::Opened => "opened",
                    GitLabMergeRequestState::Closed => "closed",
                    GitLabMergeRequestState::All => "all",
                });
                query.append_pair("per_page", "50");
                query.append_pair("page", &page.max(1).to_string());
                if let Some(reviewer_id) = reviewer_id {
                    query.append_pair("reviewer_id", &reviewer_id.to_string());
                }
                query.append_pair("order_by", "updated_at");
                query.append_pair("sort", "asc");
                if let Some(updated_after) = updated_after {
                    safe_timestamp(&updated_after)?;
                    query.append_pair("updated_after", &updated_after);
                }
            }
            let path = format!("{}?{}", url.path(), url.query().ok_or(Error::Input)?);
            Ok((Method::GET, path, None))
        }
        Op::GetMergeRequest { project_path, merge_request_iid, include_changes } => {
            if merge_request_iid == 0 { return Err(Error::Input); }
            let suffix = if include_changes { "/changes" } else { "" };
            Ok((Method::GET, format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}{suffix}", project_segment(&project_path)?), None))
        }
        Op::ListMergeRequestNotes { project_path, merge_request_iid, page } => {
            if merge_request_iid == 0 { return Err(Error::Input); }
            Ok((Method::GET, format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}/notes?per_page=50&page={}&order_by=created_at&sort=desc", project_segment(&project_path)?, page.max(1)), None))
        }
        Op::CreateMergeRequest { project_path, source_branch, target_branch, title, description, draft } => {
            if source_branch.is_empty() || target_branch.is_empty() || source_branch.len() > 255 || target_branch.len() > 255 || description.len() > 64 * 1024 {
                return Err(Error::Input);
            }
            Ok((Method::POST, format!("/api/v4/projects/{}/merge_requests", project_segment(&project_path)?), Some(json!({"source_branch":source_branch,"target_branch":target_branch,"title":draft_title(title,draft)?,"description":description,"draft":draft,"remove_source_branch":false,"squash":false}))))
        }
        Op::UpdateMergeRequest { project_path, merge_request_iid, title, description, draft, target_branch } => {
            if merge_request_iid == 0 || description.len() > 64 * 1024 { return Err(Error::Input); }
            if target_branch.as_ref().is_some_and(|branch| branch.is_empty() || branch.len() > 255 || branch.starts_with('-') || branch.chars().any(char::is_control)) {
                return Err(Error::Input);
            }
            let mut body = json!({"title":draft_title(title,draft)?,"description":description,"draft":draft});
            if let Some(target_branch) = target_branch {
                body["target_branch"] = Value::String(target_branch);
            }
            Ok((Method::PUT, format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}", project_segment(&project_path)?), Some(body)))
        }
        Op::CreateMergeRequestDiscussion { project_path, merge_request_iid, body, position } => {
            if merge_request_iid == 0 { return Err(Error::Input); }
            validate_discussion(&position, &body)?;
            Ok((Method::POST, format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}/discussions", project_segment(&project_path)?), Some(json!({"body":body,"position":{"position_type":"text","base_sha":position.base_sha,"start_sha":position.start_sha,"head_sha":position.head_sha,"old_path":position.old_path,"new_path":position.new_path,"old_line":position.old_line,"new_line":position.new_line}}))))
        }
        Op::CreateMergeRequestDiscussionReply { project_path, merge_request_iid, discussion_id, body } => {
            if merge_request_iid == 0
                || discussion_id.is_empty()
                || discussion_id.len() > 128
                || !discussion_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || body.trim().is_empty()
                || body.len() > 32 * 1024
            {
                return Err(Error::Input);
            }
            Ok((
                Method::POST,
                format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}/discussions/{discussion_id}/notes", project_segment(&project_path)?),
                Some(json!({"body": body})),
            ))
        }
        Op::ListMergeRequestDiscussions { project_path, merge_request_iid, page } => {
            if merge_request_iid == 0 { return Err(Error::Input); }
            Ok((Method::GET, format!("/api/v4/projects/{}/merge_requests/{merge_request_iid}/discussions?per_page=50&page={}", project_segment(&project_path)?, page.max(1)), None))
        }
    }
}

async fn execute(
    authority: &impl TrustedAuthority,
    lease: AuthorizedUse,
    method: Method,
    url: Url,
    body: Option<Value>,
    host: String,
) -> Result<JsonResult, Error> {
    authority.audit_before_act(&lease.evidence).map_err(|_| Error::Audit)?;
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 443))
        .await
        .map_err(|_| Error::Destination)?
        .collect();
    if addresses.is_empty() || addresses.len() > 64 || addresses.iter().any(|address| !safe_unicast(address.ip())) {
        return Err(Error::Destination);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(GITLAB_DEADLINE)
        .connect_timeout(Duration::from_secs(5))
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|_| Error::Upstream)?;
    let secret = authority.resolve_for_use(&lease.evidence).map_err(|_| Error::Credential)?;
    let raw = secret.expose_to_mediator();
    let buffer = Zeroizing::new(raw.to_owned());
    let mut header = HeaderValue::from_str(&buffer).map_err(|_| Error::Credential)?;
    header.set_sensitive(true);
    let mut request = client.request(method, url).header("PRIVATE-TOKEN", header);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|_| Error::Upstream)?;
    super::read_json_limited(response, raw, MAX_GITLAB_BODY).await
}

fn safe_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            let [a, b, c, _] = octets;
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && a != 0
                && a < 240
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && (b == 0 || (b == 88 && c == 99) || b == 2))
                && !(a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unicast_link_local()
                && ip.to_ipv4_mapped().is_none()
                && {
                    let segments = ip.segments();
                    (segments[0] & 0xfe00) == 0xfc00
                        || ((segments[0] & 0xe000) == 0x2000
                            && !(segments[0] == 0x2001 && segments[1] < 0x200)
                            && !(segments[0] == 0x2001 && segments[1] == 0xdb8)
                            && segments[0] != 0x2002
                            && segments[0] != 0x3fff)
                }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_build_only_gitlab_v4_paths_and_fixed_methods() {
        let (method, path, body) = operation_request(GitLabOperation::UpdateIssue {
            project_path: "group/sub/project".into(),
            issue_iid: 7,
            title: Some("New title".into()),
            description: None,
            labels: None,
            state_event: None,
        })
        .expect("valid operation");
        assert_eq!(method, Method::PUT);
        assert_eq!(path, "/api/v4/projects/group%2Fsub%2Fproject/issues/7");
        assert_eq!(body.expect("write body")["title"], "New title");

        assert!(operation_request(GitLabOperation::GetIssue {
            project_path: "group/../private".into(),
            issue_iid: 7,
        })
        .is_err());
        assert!(operation_request(GitLabOperation::UpdateIssue {
            project_path: "group/project".into(),
            issue_iid: 0,
            title: Some("bad".into()),
            description: None,
            labels: None,
            state_event: None,
        })
        .is_err());

        let (_, _, close_body) = operation_request(GitLabOperation::UpdateIssue {
            project_path: "group/project".into(),
            issue_iid: 7,
            title: None,
            description: None,
            labels: None,
            state_event: Some(oqto_protocol::gitlab::GitLabIssueStateEvent::Close),
        })
        .expect("state-only issue transition");
        assert_eq!(close_body.expect("state transition body")["state_event"], "close");

        let (_, notes_path, _) = operation_request(GitLabOperation::ListMergeRequestNotes {
            project_path: "group/sub/project".into(),
            merge_request_iid: 12,
            page: 2,
        })
        .expect("MR notes operation");
        assert!(notes_path.contains("/merge_requests/12/notes?per_page=50&page=2"));

        let (_, reply_path, _) = operation_request(GitLabOperation::CreateMergeRequestDiscussionReply {
            project_path: "group/project".into(),
            merge_request_iid: 12,
            discussion_id: "abcdef_123".into(),
            body: "reply".into(),
        })
        .expect("discussion reply operation");
        assert!(reply_path.ends_with("/merge_requests/12/discussions/abcdef_123/notes"));
        assert!(operation_request(GitLabOperation::CreateMergeRequestDiscussionReply {
            project_path: "group/project".into(),
            merge_request_iid: 12,
            discussion_id: "../private".into(),
            body: "reply".into(),
        }).is_err());
    }

    #[test]
    fn private_gitlab_dns_is_allowed_but_local_and_unsafe_addresses_are_not() {
        assert!(safe_unicast("10.20.30.40".parse().unwrap()));
        assert!(safe_unicast("192.168.1.20".parse().unwrap()));
        assert!(safe_unicast("172.20.1.20".parse().unwrap()));
        assert!(safe_unicast("fd00::10".parse().unwrap()));
        assert!(!safe_unicast("127.0.0.1".parse().unwrap()));
        assert!(!safe_unicast("169.254.1.1".parse().unwrap()));
        assert!(!safe_unicast("::1".parse().unwrap()));
        assert!(!safe_unicast("fe80::1".parse().unwrap()));
        assert!(validate_host("gitlab.corp.internal").is_ok());
        assert!(validate_host("127.0.0.1").is_err());
        assert!(validate_path_prefix("/gitlab/nested").is_ok());
        for prefix in ["relative", "/a/../b", "/a%2fb", "/a//b", "/a/"] {
            assert!(validate_path_prefix(prefix).is_err(), "{prefix}");
        }
    }
}
