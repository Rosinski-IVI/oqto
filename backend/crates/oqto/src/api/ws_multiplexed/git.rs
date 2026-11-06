//! Git channel: semantic version-control commands over the multiplexed
//! WebSocket.
//!
//! Every command runs `git` as the work directory's owner, in the work
//! directory itself, after the same ownership gate the trx channel uses. The
//! surface is semantic — status, diff, log, stage, unstage, commit — never a
//! general exec: the caller cannot choose the binary, the flags, or the
//! directory beyond a work directory it already owns.
//!
//! Listings are capped. A repository with tens of thousands of changed paths
//! must not be able to stall the connection or flood the client, so the
//! handler reports what it truncated rather than trying to send everything.

use super::*;
use crate::api::handlers::trx::validate_workspace_path;
use std::path::Path;

/// Beyond this many changed paths the status listing is truncated.
const MAX_STATUS_ENTRIES: usize = 500;
/// Beyond this many bytes a diff is truncated.
const MAX_DIFF_BYTES: usize = 256 * 1024;
/// Commits returned when the caller does not ask for a specific number.
const DEFAULT_LOG_LIMIT: usize = 50;
const MAX_LOG_LIMIT: usize = 500;

/// Field and record separators for `git log`; neither appears in git output.
const FIELD: char = '\u{1f}';

struct GitOutput {
    stdout: String,
}

/// Environment for network commands. Git must fail rather than wait: a
/// prompt for a passphrase, a credential, or an unknown host key would hang
/// the request until the transport's own timeout and tell the caller nothing.
fn non_interactive_env() -> serde_json::Value {
    serde_json::json!({
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_SSH_COMMAND": "ssh -oBatchMode=yes",
    })
}

/// Runs one git invocation in a validated work directory as its owner.
async fn run_git(
    state: &AppState,
    user_id: &str,
    workspace: &Path,
    args: Vec<String>,
) -> Result<GitOutput, String> {
    run_git_with_env(state, user_id, workspace, args, serde_json::json!({})).await
}

async fn run_git_with_env(
    state: &AppState,
    user_id: &str,
    workspace: &Path,
    args: Vec<String>,
    env: serde_json::Value,
) -> Result<GitOutput, String> {
    if let Some(linux_users) = state.linux_users.as_ref().filter(|cfg| cfg.enabled) {
        let linux_username = linux_users.linux_username(user_id);
        let cwd = workspace.to_string_lossy().to_string();
        let data = tokio::task::spawn_blocking(move || {
            crate::local::linux_users::usermgr_request_with_data(
                "run-as-user",
                serde_json::json!({
                    "username": linux_username,
                    "binary": "git",
                    "args": args,
                    "env": env,
                    "cwd": cwd,
                }),
            )
        })
        .await
        .map_err(|e| format!("task join error: {e}"))?
        .map_err(|e| e.to_string())?;
        let stdout = data
            .as_ref()
            .and_then(|value| value.get("stdout"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(GitOutput { stdout })
    } else {
        let mut command = tokio::process::Command::new("git");
        command.args(&args).current_dir(workspace);
        if let Some(vars) = env.as_object() {
            for (key, value) in vars {
                if let Some(value) = value.as_str() {
                    command.env(key, value);
                }
            }
        }
        let output = command
            .output()
            .await
            .map_err(|e| format!("failed to execute git: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        })
    }
}

/// Push only the committed current feature branch to a remote that resolves to
/// the workspace's configured GitLab project. Errors deliberately omit Git's
/// stderr because it can contain a credential-bearing remote URL.
pub(crate) async fn push_current_branch(
    state: &AppState,
    user_id: &str,
    workspace_path: &str,
    remote_name: &str,
    gitlab_host: &str,
    gitlab_path_prefix: &str,
    project_path: &str,
    default_branch: &str,
) -> Result<String, String> {
    if !valid_remote_name(remote_name) {
        return Err("Choose a valid Git remote for this GitLab project".into());
    }
    let workspace = validate_workspace_path(state, user_id, workspace_path)
        .await
        .map_err(|_| "GitLab workspace is unavailable".to_owned())?;
    let remote_url = run_git(
        state,
        user_id,
        &workspace,
        vec!["remote".into(), "get-url".into(), "--fetch".into(), remote_name.into()],
    )
    .await
    .map_err(|_| "The selected Git remote is unavailable".to_owned())?;
    let actual_project = crate::gitlab::project_from_remote(
        remote_url.stdout.trim(),
        gitlab_host,
        gitlab_path_prefix,
    )
    .map_err(|_| "The selected Git remote does not match the mapped GitLab project".to_owned())?;
    if actual_project != project_path {
        return Err("The selected Git remote does not match the mapped GitLab project".into());
    }

    let branch = run_git(
        state,
        user_id,
        &workspace,
        vec!["branch".into(), "--show-current".into()],
    )
    .await
    .map_err(|_| "Unable to read the current Git branch".to_owned())?
    .stdout
    .trim()
    .to_owned();
    if branch.is_empty() || branch == default_branch || branch.starts_with('-') {
        return Err("Draft MRs require a non-default feature branch".into());
    }
    run_git(
        state,
        user_id,
        &workspace,
        vec!["rev-parse".into(), "--verify".into(), "HEAD".into()],
    )
    .await
    .map_err(|_| "Commit at least one change on the feature branch before requesting a draft MR".to_owned())?;
    let status = run_git(
        state,
        user_id,
        &workspace,
        vec![
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=normal".into(),
        ],
    )
    .await
    .map_err(|_| "Unable to check the Git worktree".to_owned())?;
    if !status.stdout.is_empty() {
        return Err("Commit the intended changes before requesting a draft MR".into());
    }

    run_git_with_env(
        state,
        user_id,
        &workspace,
        vec![
            "push".into(),
            "--set-upstream".into(),
            remote_name.into(),
            branch.clone(),
        ],
        non_interactive_env(),
    )
    .await
    .map_err(|_| {
        "Git push failed. Check the workspace's normal Git credentials and remote permissions; Oqto did not use the GitLab API token for Git transport.".to_owned()
    })?;
    Ok(branch)
}

/// Bounded, credential-free Git context for the selected GitLab project.
pub(crate) async fn inspect_gitlab_workspace(
    state: &AppState,
    user_id: &str,
    workspace_path: &str,
    gitlab_host: &str,
    gitlab_path_prefix: &str,
    project_path: &str,
) -> Result<serde_json::Value, String> {
    let workspace = validate_workspace_path(state, user_id, workspace_path)
        .await
        .map_err(|_| "GitLab workspace is unavailable".to_owned())?;
    let branch = run_git(
        state,
        user_id,
        &workspace,
        vec!["branch".into(), "--show-current".into()],
    )
    .await
    .map_err(|_| "Unable to read the current Git branch".to_owned())?
    .stdout
    .trim()
    .to_owned();
    let status = run_git(
        state,
        user_id,
        &workspace,
        vec![
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=normal".into(),
        ],
    )
    .await
    .map_err(|_| "Unable to inspect the Git worktree".to_owned())?;
    let remotes = run_git(state, user_id, &workspace, vec!["remote".into()])
        .await
        .map_err(|_| "Unable to list Git remotes".to_owned())?;
    let mut matching_remotes = Vec::new();
    for name in remotes.stdout.lines().map(str::trim).filter(|name| valid_remote_name(name)) {
        let url = run_git(
            state,
            user_id,
            &workspace,
            vec!["remote".into(), "get-url".into(), "--fetch".into(), name.into()],
        )
        .await
        .map_err(|_| "Unable to inspect a Git remote".to_owned())?;
        if crate::gitlab::project_from_remote(
            url.stdout.trim(),
            gitlab_host,
            gitlab_path_prefix,
        )
        .is_ok_and(|remote_project| remote_project == project_path)
        {
            matching_remotes.push(name.to_owned());
        }
    }
    matching_remotes.sort();
    Ok(serde_json::json!({
        "current_branch": branch,
        "worktree_clean": status.stdout.is_empty(),
        "matching_remote_names": matching_remotes,
    }))
}

fn valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
        })
        && !name.split('/').any(|part| matches!(part, "." | ".."))
}

/// `## main...origin/main [ahead 2, behind 1]` -> branch, upstream, distance.
fn parse_branch_header(line: &str) -> (String, Option<String>, usize, usize) {
    let rest = line.trim_start_matches("## ");
    let (refs, distance) = match rest.split_once(" [") {
        Some((refs, tail)) => (refs, tail.trim_end_matches(']')),
        None => (rest, ""),
    };
    let (branch, upstream) = match refs.split_once("...") {
        Some((branch, upstream)) => (branch.to_string(), Some(upstream.to_string())),
        None => (refs.to_string(), None),
    };
    let count = |key: &str| -> usize {
        distance
            .split(", ")
            .find_map(|part| part.strip_prefix(key))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    };
    (branch, upstream, count("ahead "), count("behind "))
}

/// Reads `git status --porcelain=v1 -z`, whose records are NUL separated and
/// whose renames carry the old path as the following record.
fn parse_status(stdout: &str) -> (String, Option<String>, usize, usize, Vec<GitStatusEntry>) {
    let mut branch = String::new();
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut entries = Vec::new();
    let mut records = stdout.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("## ") {
            let (name, up, a, b) = parse_branch_header(&format!("## {header}"));
            branch = name;
            upstream = up;
            ahead = a;
            behind = b;
            continue;
        }
        if record.len() < 4 {
            continue;
        }
        let (codes, path) = record.split_at(3);
        let index = codes[0..1].to_string();
        let worktree = codes[1..2].to_string();
        // A rename spends two records: the new path, then the old one.
        let renamed_from = if index == "R" || worktree == "R" {
            records.next().map(|old| old.to_string())
        } else {
            None
        };
        entries.push(GitStatusEntry {
            path: path.to_string(),
            index,
            worktree,
            renamed_from,
        });
    }
    (branch, upstream, ahead, behind, entries)
}

/// Reads `git for-each-ref --format=%(refname:short)%00%(worktreepath)`.
/// The worktree path is empty for a branch no checkout holds.
fn parse_branches(stdout: &str, current: &str) -> Vec<GitBranch> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (name, worktree) = line.split_once('\0').unwrap_or((line, ""));
            GitBranch {
                name: name.to_string(),
                current: name == current,
                worktree: if worktree.trim().is_empty() {
                    None
                } else {
                    Some(worktree.to_string())
                },
            }
        })
        .collect()
}

fn parse_log(stdout: &str) -> Vec<GitCommitSummary> {
    stdout
        .split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split(FIELD);
            let id = fields.next()?.to_string();
            let short_id = fields.next()?.to_string();
            let summary = fields.next()?.to_string();
            let author = fields.next()?.to_string();
            let timestamp = fields.next()?.trim().parse().unwrap_or(0);
            Some(GitCommitSummary {
                id,
                short_id,
                summary,
                author,
                timestamp,
            })
        })
        .collect()
}

fn parse_remotes(stdout: &str) -> Vec<super::GitRemoteEntry> {
    let mut remotes = Vec::new();
    let mut names = std::collections::HashSet::new();
    for line in stdout.lines() {
        let Some((name, value)) = line.split_once('\t') else {
            continue;
        };
        let Some(url) = value.strip_suffix(" (fetch)") else {
            continue;
        };
        let Some(url) = sanitize_remote_url(url) else {
            continue;
        };
        if names.insert(name.to_owned()) {
            remotes.push(super::GitRemoteEntry {
                name: name.to_owned(),
                url,
            });
        }
    }
    remotes
}

fn sanitize_remote_url(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        if !valid_remote_host(host) || path.is_empty() || path.contains(['?', '#', '\\']) {
            return None;
        }
        return Some(format!("git@{host}:{path}"));
    }
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "https" | "ssh")
        || url.host_str().is_none_or(|host| !valid_remote_host(host))
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.to_string())
}

fn valid_remote_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.parse::<std::net::IpAddr>().is_err()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
}

fn error(id: Option<String>, message: impl Into<String>) -> Option<WsEvent> {
    Some(WsEvent::Git(GitWsEvent::Error {
        id,
        error: message.into(),
    }))
}

/// Handle git channel commands.
pub(super) async fn handle_git_command(
    cmd: GitWsCommand,
    user_id: &str,
    state: &AppState,
) -> Option<WsEvent> {
    let (id, workspace_path) = match &cmd {
        GitWsCommand::Status { id, workspace_path }
        | GitWsCommand::Diff {
            id, workspace_path, ..
        }
        | GitWsCommand::Log {
            id, workspace_path, ..
        }
        | GitWsCommand::Stage {
            id, workspace_path, ..
        }
        | GitWsCommand::Unstage {
            id, workspace_path, ..
        }
        | GitWsCommand::Commit {
            id, workspace_path, ..
        }
        | GitWsCommand::Branches { id, workspace_path }
        | GitWsCommand::Remotes { id, workspace_path }
        | GitWsCommand::Switch {
            id, workspace_path, ..
        }
        | GitWsCommand::Fetch { id, workspace_path }
        | GitWsCommand::Pull { id, workspace_path }
        | GitWsCommand::Push { id, workspace_path } => (id.clone(), workspace_path.clone()),
    };

    let workspace = match validate_workspace_path(state, user_id, &workspace_path).await {
        Ok(path) => path,
        Err(err) => return error(id, err.to_string()),
    };

    // Status/diff/log/branches are reads; everything else changes the
    // repository (index, working tree, refs) or the remote.
    let requires_write = !matches!(
        cmd,
        GitWsCommand::Status { .. }
            | GitWsCommand::Diff { .. }
            | GitWsCommand::Log { .. }
            | GitWsCommand::Branches { .. }
            | GitWsCommand::Remotes { .. }
    );
    if requires_write
        && let Err(err) =
            require_workspace_role(state, user_id, &workspace_path, WorkspaceNeed::Write).await
    {
        return error(id, err);
    }

    match cmd {
        GitWsCommand::Status { id, .. } => {
            let args = vec![
                "status".to_string(),
                "--porcelain=v1".to_string(),
                "-z".to_string(),
                "--branch".to_string(),
            ];
            match run_git(state, user_id, &workspace, args).await {
                Ok(output) => {
                    let (branch, upstream, ahead, behind, mut entries) =
                        parse_status(&output.stdout);
                    let truncated = entries.len() > MAX_STATUS_ENTRIES;
                    entries.truncate(MAX_STATUS_ENTRIES);
                    Some(WsEvent::Git(GitWsEvent::StatusResult {
                        id,
                        branch,
                        upstream,
                        ahead,
                        behind,
                        entries,
                        truncated,
                    }))
                }
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Diff {
            id, path, staged, ..
        } => {
            let mut args = vec!["diff".to_string(), "--no-color".to_string()];
            if staged {
                args.push("--cached".to_string());
            }
            // Everything after `--` is a path, never a flag.
            args.push("--".to_string());
            args.push(path.clone());
            match run_git(state, user_id, &workspace, args).await {
                Ok(output) => {
                    let truncated = output.stdout.len() > MAX_DIFF_BYTES;
                    let mut patch = output.stdout;
                    if truncated {
                        patch.truncate(
                            (0..=MAX_DIFF_BYTES)
                                .rev()
                                .find(|index| patch.is_char_boundary(*index))
                                .unwrap_or(0),
                        );
                    }
                    Some(WsEvent::Git(GitWsEvent::DiffResult {
                        id,
                        path,
                        staged,
                        patch,
                        truncated,
                    }))
                }
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Log { id, limit, .. } => {
            let count = limit.unwrap_or(DEFAULT_LOG_LIMIT).clamp(1, MAX_LOG_LIMIT);
            let args = vec![
                "log".to_string(),
                format!("-n{count}"),
                "-z".to_string(),
                format!("--format=%H{FIELD}%h{FIELD}%s{FIELD}%an{FIELD}%ct"),
            ];
            match run_git(state, user_id, &workspace, args).await {
                Ok(output) => Some(WsEvent::Git(GitWsEvent::LogResult {
                    id,
                    commits: parse_log(&output.stdout),
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Stage { id, paths, .. } => {
            if paths.is_empty() {
                return error(id, "no paths to stage");
            }
            let mut args = vec!["add".to_string(), "--".to_string()];
            args.extend(paths);
            match run_git(state, user_id, &workspace, args).await {
                Ok(_) => Some(WsEvent::Git(GitWsEvent::StageResult { id, success: true })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Unstage { id, paths, .. } => {
            if paths.is_empty() {
                return error(id, "no paths to unstage");
            }
            let mut args = vec![
                "restore".to_string(),
                "--staged".to_string(),
                "--".to_string(),
            ];
            args.extend(paths);
            match run_git(state, user_id, &workspace, args).await {
                Ok(_) => Some(WsEvent::Git(GitWsEvent::UnstageResult {
                    id,
                    success: true,
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Commit { id, message, .. } => {
            if message.trim().is_empty() {
                return error(id, "commit message is empty");
            }
            let args = vec![
                "commit".to_string(),
                "-m".to_string(),
                message,
                "--no-verify".to_string(),
            ];
            if let Err(err) = run_git(state, user_id, &workspace, args).await {
                return error(id, err);
            }
            match run_git(
                state,
                user_id,
                &workspace,
                vec!["rev-parse".to_string(), "HEAD".to_string()],
            )
            .await
            {
                Ok(output) => Some(WsEvent::Git(GitWsEvent::CommitResult {
                    id,
                    commit: output.stdout.trim().to_string(),
                    success: true,
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Branches { id, .. } => {
            let current = run_git(
                state,
                user_id,
                &workspace,
                vec!["branch".to_string(), "--show-current".to_string()],
            )
            .await
            .map(|output| output.stdout.trim().to_string())
            .unwrap_or_default();
            let args = vec![
                "for-each-ref".to_string(),
                "--format=%(refname:short)%00%(worktreepath)".to_string(),
                "refs/heads".to_string(),
            ];
            match run_git(state, user_id, &workspace, args).await {
                Ok(output) => Some(WsEvent::Git(GitWsEvent::BranchesResult {
                    id,
                    branches: parse_branches(&output.stdout, &current),
                    current,
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Remotes { id, .. } => {
            match run_git(
                state,
                user_id,
                &workspace,
                vec!["remote".to_string(), "-v".to_string()],
            )
            .await
            {
                Ok(output) => Some(WsEvent::Git(GitWsEvent::RemotesResult {
                    id,
                    remotes: parse_remotes(&output.stdout),
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Switch { id, branch, .. } => {
            // A name that could read as a flag never reaches git.
            if branch.starts_with('-') || branch.trim().is_empty() {
                return error(id, "invalid branch name");
            }
            // An Agent may be working in this tree. Moving it under a dirty
            // working copy is refused outright rather than left to git's own
            // narrower check, which permits changes it can carry across.
            match run_git(
                state,
                user_id,
                &workspace,
                vec![
                    "status".to_string(),
                    "--porcelain=v1".to_string(),
                    "-z".to_string(),
                ],
            )
            .await
            {
                Ok(output) if !output.stdout.trim().is_empty() => {
                    return error(id, "the working tree has changes; commit or stash first");
                }
                Err(err) => return error(id, err),
                _ => {}
            }
            match run_git(
                state,
                user_id,
                &workspace,
                vec!["switch".to_string(), branch.clone()],
            )
            .await
            {
                Ok(_) => Some(WsEvent::Git(GitWsEvent::SwitchResult {
                    id,
                    branch,
                    success: true,
                })),
                Err(err) => error(id, err),
            }
        }
        GitWsCommand::Fetch { id, .. } => {
            remote_result(
                state,
                user_id,
                &workspace,
                id,
                "fetch",
                vec!["fetch".to_string()],
            )
            .await
        }
        GitWsCommand::Pull { id, .. } => {
            remote_result(
                state,
                user_id,
                &workspace,
                id,
                "pull",
                vec!["pull".to_string(), "--ff-only".to_string()],
            )
            .await
        }
        GitWsCommand::Push { id, .. } => {
            // Push where the branch already points; a branch with no upstream
            // gets one on its default remote rather than a confusing failure.
            let upstream = run_git(
                state,
                user_id,
                &workspace,
                vec![
                    "rev-parse".to_string(),
                    "--abbrev-ref".to_string(),
                    "--symbolic-full-name".to_string(),
                    "@{upstream}".to_string(),
                ],
            )
            .await
            .ok()
            .map(|output| output.stdout.trim().to_string())
            .filter(|value| !value.is_empty());
            let args = if upstream.is_some() {
                vec!["push".to_string()]
            } else {
                let remote = default_remote(state, user_id, &workspace).await?;
                let branch = run_git(
                    state,
                    user_id,
                    &workspace,
                    vec!["branch".to_string(), "--show-current".to_string()],
                )
                .await
                .map(|output| output.stdout.trim().to_string())
                .unwrap_or_default();
                if branch.is_empty() {
                    return error(id, "cannot push a detached HEAD");
                }
                vec![
                    "push".to_string(),
                    "--set-upstream".to_string(),
                    remote,
                    branch,
                ]
            };
            remote_result(state, user_id, &workspace, id, "push", args).await
        }
    }
}

/// The remote a push should adopt: origin when it exists, else the first.
async fn default_remote(state: &AppState, user_id: &str, workspace: &Path) -> Option<String> {
    let output = run_git(state, user_id, workspace, vec!["remote".to_string()])
        .await
        .ok()?;
    let remotes: Vec<&str> = output.stdout.lines().filter(|l| !l.is_empty()).collect();
    if remotes.is_empty() {
        return None;
    }
    Some(
        remotes
            .iter()
            .find(|remote| **remote == "origin")
            .unwrap_or(&remotes[0])
            .to_string(),
    )
}

/// Runs a network command non-interactively and reports what git said.
async fn remote_result(
    state: &AppState,
    user_id: &str,
    workspace: &Path,
    id: Option<String>,
    operation: &str,
    args: Vec<String>,
) -> Option<WsEvent> {
    match run_git_with_env(state, user_id, workspace, args, non_interactive_env()).await {
        Ok(output) => Some(WsEvent::Git(GitWsEvent::RemoteResult {
            id,
            operation: operation.to_string(),
            summary: output.stdout.trim().to_string(),
            success: true,
        })),
        Err(err) => error(id, err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_branch_upstream_and_distance() {
        let (branch, upstream, ahead, behind) =
            parse_branch_header("## main...origin/main [ahead 2, behind 1]");
        assert_eq!(branch, "main");
        assert_eq!(upstream.as_deref(), Some("origin/main"));
        assert_eq!((ahead, behind), (2, 1));

        let (branch, upstream, ahead, behind) = parse_branch_header("## solo");
        assert_eq!(branch, "solo");
        assert_eq!(upstream, None);
        assert_eq!((ahead, behind), (0, 0));
    }

    #[test]
    fn remote_listing_never_returns_embedded_credentials_or_push_only_urls() {
        let remotes = parse_remotes(
            "origin\thttps://alice:secret@gitlab.example/team/repo.git (fetch)\n\
             origin\thttps://alice:secret@gitlab.example/team/repo.git (push)\n\
             local\tfile:///tmp/repo (fetch)\n",
        );
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].url, "https://gitlab.example/team/repo.git");
        assert!(!remotes[0].url.contains("secret"));
    }

    #[test]
    fn keeps_index_and_worktree_status_distinct() {
        let stdout = "## main\0M  staged.rs\0 M dirty.rs\0MM both.rs\0?? new.rs\0";
        let (branch, _, _, _, entries) = parse_status(stdout);
        assert_eq!(branch, "main");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].path, "staged.rs");
        assert_eq!(
            (entries[0].index.as_str(), entries[0].worktree.as_str()),
            ("M", " ")
        );
        assert_eq!(
            (entries[1].index.as_str(), entries[1].worktree.as_str()),
            (" ", "M")
        );
        assert_eq!(
            (entries[2].index.as_str(), entries[2].worktree.as_str()),
            ("M", "M")
        );
        assert_eq!(entries[3].index, "?");
    }

    #[test]
    fn a_rename_carries_the_path_it_came_from() {
        let stdout = "## main\0R  new/name.rs\0old/name.rs\0 M other.rs\0";
        let (_, _, _, _, entries) = parse_status(stdout);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "new/name.rs");
        assert_eq!(entries[0].renamed_from.as_deref(), Some("old/name.rs"));
        // The old path is consumed by the rename, not read as another entry.
        assert_eq!(entries[1].path, "other.rs");
    }

    #[test]
    fn reads_branches_and_the_checkout_that_holds_each() {
        let stdout = "main\0/home/u/repo\nfeature\0\nspike\0/home/u/wt\n";
        let branches = parse_branches(stdout, "main");
        assert_eq!(branches.len(), 3);
        assert!(branches[0].current);
        assert_eq!(branches[0].worktree.as_deref(), Some("/home/u/repo"));
        // A branch no checkout holds is free to switch to.
        assert!(!branches[1].current);
        assert_eq!(branches[1].worktree, None);
        assert_eq!(branches[2].worktree.as_deref(), Some("/home/u/wt"));
    }

    #[test]
    fn network_commands_never_wait_for_a_prompt() {
        let env = non_interactive_env();
        assert_eq!(env["GIT_TERMINAL_PROMPT"], "0");
        assert!(
            env["GIT_SSH_COMMAND"]
                .as_str()
                .unwrap_or_default()
                .contains("BatchMode=yes")
        );
    }

    #[test]
    fn reads_commits_field_by_field() {
        let stdout = format!(
            "abc123{FIELD}abc12{FIELD}Fix the thing{FIELD}Ada{FIELD}1700000000\0\
             def456{FIELD}def45{FIELD}Add a thing{FIELD}Grace{FIELD}1700000100\0"
        );
        let commits = parse_log(&stdout);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].summary, "Fix the thing");
        assert_eq!(commits[0].author, "Ada");
        assert_eq!(commits[0].timestamp, 1_700_000_000);
        assert_eq!(commits[1].short_id, "def45");
    }
}
