---
description: "Plan and refine Oqto's GitLab Community integration for tickets, merge requests, reviews, and comment-driven updates."
name: "GitLab Agent Workflow Plan"
argument-hint: "Describe what to refine in the integration plan."
agent: "plan"
---

## Plan: GitLab Community Agent Workflow

Build a GitLab Community integration around the standard GitLab REST API: an Oqto backend service handles credentials, API access, and polling; a Pi extension gives the agent scoped tools; Oqto UI provides setup, notifications, and approvals. This supports private GitLab without exposing tokens to the agent or browser, and does not rely on GitLab Duo.

**Steps**

**Phase 1: Connection and API service**
1. Support one GitLab instance with multiple projects. Map a workspace from its Git remote, with an explicit project override.
2. Add a backend-only GitLab client for issues, notes, merge requests, diffs, and discussions. Extend private-host HTTPS/TLS access only for the configured GitLab host; do not make arbitrary URLs accessible.
3. Poll configurable event categories: assigned/authored issues, mentions/review requests, and broader project activity. Persist cursors and deduplicate events.

**Phase 2: Tickets**
4. Add a GitLab issue adapter and inbox alongside the local TRX tracker. Reuse its UI patterns without replacing its behavior.
5. Support ticket planning and discussion in an agent session. Require approval to start implementation. Present ticket edits and comments as proposals for individual approval.

**Phase 3: Merge requests and review**
6. Add a Pi extension that uses scoped Oqto operations and the active workspace’s project mapping. Keep tokens out of prompts and extension configuration.
7. After implementation starts, allow the agent to create or update a draft MR tied to the ticket. Require confirmation before marking it ready.
8. Let the agent draft inline reviews, with confirmation before submission and validation against the current diff. Connect new discussion comments to the ticket/session so the user can choose to address them and update the draft MR.

**Phase 4: Verification**
9. Add focused backend and frontend tests, plus a manual workflow test against a disposable GitLab CE project.

**Relevant files**
- [credentials/policy.rs](backend/crates/oqto/src/credentials/policy.rs) and [connections.rs](backend/crates/oqto/src/api/handlers/connections.rs): existing connection and host policies.
- [oqto-credential-http/lib.rs](backend/crates/oqto-credential-http/src/lib.rs): current public-host, GET-only behavior that should not be reused unchanged for GitLab writes.
- [history.rs](backend/crates/oqto/src/api/ws_multiplexed/history.rs) and [git.rs](backend/crates/oqto/src/api/ws_multiplexed/git.rs): local issue and repository-operation patterns.
- [agent-instructions.md](backend/crates/oqto/src/templates/embedded/agent-instructions.md) and [06-pi-extensions.sh](scripts/setup/06-pi-extensions.sh): Pi extension guidance and setup.
- [issues-contract.ts](frontend/src/oqto-ui/platform/issues-contract.ts), [mux-issues.ts](frontend/src/oqto-ui/platform/mux-issues.ts), [TrxView.tsx](frontend/features/sessions/components/TrxView.tsx), and [SessionScreen.tsx](frontend/features/sessions/SessionScreen.tsx): issue adapters, UI, and start-to-session flow to build on.
- [oqto-ui-issues.test.tsx](frontend/tests/issues/oqto-ui-issues.test.tsx), [oqto-ui-git.test.tsx](frontend/tests/git/oqto-ui-git.test.tsx), and [credentials/tests.rs](backend/crates/oqto/src/credentials/tests.rs): nearby test patterns.

**Verification**
1. Backend: `cd backend && cargo test -p oqto`, including GitLab API, polling/deduplication, host policy, and secret-redaction tests.
2. Frontend: `cd frontend && bun run test:run -- tests/issues/oqto-ui-issues.test.tsx tests/git/oqto-ui-git.test.tsx`, extended with GitLab workflow tests.
3. Typecheck: `cd frontend && bun run typecheck:oqto-ui`.
4. Manually verify connection, project mapping, configurable notifications, ticket approvals, draft MR creation/update, inline-review approval, and comment-driven updates against a disposable GitLab CE project.

**Decisions**
- First release supports one GitLab instance and multiple projects; remote-based mapping has an override.
- Notifications use polling with configurable event categories; webhooks and multiple instances are out of scope.
- Implementation start, each ticket write, review submission, and changing a draft MR to ready require confirmation. Draft MR creation and updates are allowed after implementation starts.