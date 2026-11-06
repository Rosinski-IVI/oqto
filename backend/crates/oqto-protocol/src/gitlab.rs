//! Closed GitLab REST operations used by Oqto's trusted backend/runner bridge.
//! Callers cannot choose an HTTP URL, method, or credential.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLabIssueScope {
    AssignedToMe,
    CreatedByMe,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitLabIssueState {
    Opened,
    Closed,
    #[default]
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLabIssueStateEvent {
    Close,
    Reopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLabDiscussionSide {
    Old,
    New,
}

/// Closed Pi-to-Oqto GitLab operations. The active Pi session is supplied by
/// the runner subscription, never by the extension payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitLabAgentOperation {
    ReadSelectedIssue,
    ReadSelectedIssueNotes,
    ReadLinkedMergeRequest,
    ReadLinkedMergeRequestDiscussions,
    ReadProjectContext,
    ProposeImplementationStart,
    ProposeIssueUpdate {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        labels: Option<Vec<String>>,
        #[serde(default)]
        state_event: Option<GitLabIssueStateEvent>,
    },
    ProposeIssueComment {
        body: String,
    },
    ProposeDiscussionReply {
        discussion_id: String,
        body: String,
    },
    ProposeInlineReview {
        comments: Vec<GitLabAgentReviewComment>,
    },
    CreateOrUpdateDraft {
        #[serde(default)]
        remote_name: Option<String>,
        title: String,
        description: String,
        #[serde(default)]
        target_branch: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitLabAgentReviewComment {
    pub path: String,
    pub line: u64,
    pub side: GitLabDiscussionSide,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitLabAgentRpcRequest {
    pub version: u8,
    pub request_id: String,
    pub operation: GitLabAgentOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLabMergeRequestScope {
    AssignedToMe,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitLabMergeRequestState {
    #[default]
    Opened,
    Closed,
    All,
}

/// A fixed set of GitLab REST v4 operations. Values are data only; the runner
/// derives every method and URL from this enum and the configured instance host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitLabOperation {
    CurrentUser,
    GetProject {
        project_path: String,
    },
    ListProjectLabels {
        project_path: String,
        #[serde(default)]
        page: u32,
    },
    ListIssues {
        project_path: String,
        scope: GitLabIssueScope,
        #[serde(default)]
        state: GitLabIssueState,
        #[serde(default)]
        page: u32,
        #[serde(default)]
        updated_after: Option<String>,
    },
    GetIssue {
        project_path: String,
        issue_iid: u64,
    },
    ListIssueNotes {
        project_path: String,
        issue_iid: u64,
        #[serde(default)]
        page: u32,
    },
    UpdateIssue {
        project_path: String,
        issue_iid: u64,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        labels: Option<Vec<String>>,
        #[serde(default)]
        state_event: Option<GitLabIssueStateEvent>,
    },
    CreateIssueNote {
        project_path: String,
        issue_iid: u64,
        body: String,
    },
    ListMergeRequests {
        project_path: String,
        scope: GitLabMergeRequestScope,
        #[serde(default)]
        state: GitLabMergeRequestState,
        #[serde(default)]
        reviewer_id: Option<u64>,
        #[serde(default)]
        page: u32,
        #[serde(default)]
        updated_after: Option<String>,
    },
    GetMergeRequest {
        project_path: String,
        merge_request_iid: u64,
        #[serde(default)]
        include_changes: bool,
    },
    ListMergeRequestNotes {
        project_path: String,
        merge_request_iid: u64,
        #[serde(default)]
        page: u32,
    },
    CreateMergeRequest {
        project_path: String,
        source_branch: String,
        target_branch: String,
        title: String,
        description: String,
        draft: bool,
    },
    UpdateMergeRequest {
        project_path: String,
        merge_request_iid: u64,
        title: String,
        description: String,
        draft: bool,
        #[serde(default)]
        target_branch: Option<String>,
    },
    CreateMergeRequestDiscussion {
        project_path: String,
        merge_request_iid: u64,
        body: String,
        position: GitLabDiscussionPosition,
    },
    CreateMergeRequestDiscussionReply {
        project_path: String,
        merge_request_iid: u64,
        discussion_id: String,
        body: String,
    },
    ListMergeRequestDiscussions {
        project_path: String,
        merge_request_iid: u64,
        #[serde(default)]
        page: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitLabDiscussionPosition {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
}

/// Trusted backend-to-runner request. The credential reference is resolved by
/// the runner and the host is loaded from the account's stored connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitLabHttpDispatch {
    pub account_id: String,
    pub workspace_path: String,
    pub credential: crate::credential_connections::CredentialDescriptor,
    pub host: String,
    /// Validated installation path prefix, empty for an origin-root GitLab.
    pub path_prefix: String,
    pub connection_revision: u32,
    pub audit_event_id: String,
    pub operation: GitLabOperation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_has_no_raw_url_or_method_field() {
        let mut value = serde_json::json!({
            "account_id":"alice",
            "workspace_path":"/work/project",
            "credential":{"provider_id":"local","credential_id":"gitlab","revision":"1"},
            "host":"gitlab.example",
            "path_prefix":"",
            "connection_revision":1,
            "audit_event_id":"gitlab_evt_test",
            "operation":{"operation":"current_user"}
        });
        assert!(serde_json::from_value::<GitLabHttpDispatch>(value.clone()).is_ok());
        value["url"] = serde_json::json!("https://evil.example/");
        assert!(serde_json::from_value::<GitLabHttpDispatch>(value).is_err());
    }

    #[test]
    fn agent_rpc_is_typed_and_cannot_select_an_account_or_project() {
        let value = serde_json::json!({
            "version": 1,
            "request_id": "rpc-1",
            "operation": {"operation": "read_selected_issue"}
        });
        assert!(serde_json::from_value::<GitLabAgentRpcRequest>(value.clone()).is_ok());

        let mut with_account = value.clone();
        with_account["account_id"] = serde_json::json!("other-user");
        assert!(serde_json::from_value::<GitLabAgentRpcRequest>(with_account).is_err());

        let mut with_project = value;
        with_project["operation"]["project_path"] = serde_json::json!("other/project");
        assert!(serde_json::from_value::<GitLabAgentRpcRequest>(with_project).is_err());
    }
}
