import type { SaveGitLabConfigInput } from "./gitlab-connection-contract";
import type {
	GitLabConnection,
	GitLabEventCategory,
	GitLabHost,
	GitLabInboxEvent,
	GitLabProjectMapping,
	GitLabProposal,
	GitLabProposalDraft,
	GitLabTicket,
	GitLabTicketList,
	GitLabUnreadCounts,
} from "./gitlab-contract";
import type { SaveGitLabMappingInput } from "./gitlab-mapping-contract";
import type {
	CreateGitLabDraftInput,
	MarkGitLabMergeRequestReadyInput,
} from "./gitlab-merge-request-contract";
import type { CreateGitLabProposalInput } from "./gitlab-proposal-contract";
import type { IssueStatus } from "./issues-contract";
import type { MuxSocket } from "./mux-files";

interface WireRecord {
	error?: unknown;
	host?: unknown;
	path_prefix?: unknown;
	credential_id?: unknown;
	revision?: unknown;
	enabled?: unknown;
	event_categories?: unknown;
	credential_ready?: unknown;
	connected?: unknown;
	gitlab_username?: unknown;
	workspace_path?: unknown;
	project_path?: unknown;
	source?: unknown;
	categories?: unknown;
	id?: unknown;
	category?: unknown;
	entity_kind?: unknown;
	entity_iid?: unknown;
	related_issue_iid?: unknown;
	discussion_id?: unknown;
	session_id?: unknown;
	title?: unknown;
	summary?: unknown;
	url?: unknown;
	occurred_at?: unknown;
	read_at?: unknown;
	status?: unknown;
	iid?: unknown;
	priority?: unknown;
	issue_type?: unknown;
	state?: unknown;
	parent_id?: unknown;
	blocked_by?: unknown;
	labels?: unknown;
	description?: unknown;
	web_url?: unknown;
	updated_at?: unknown;
	status_conflict?: unknown;
	merge_request_iid?: unknown;
	kind?: unknown;
	issue_iid?: unknown;
	payload?: unknown;
	created_at?: unknown;
	session_stale?: unknown;
	username?: unknown;
	credential_revision?: unknown;
	issues?: unknown;
	truncated?: unknown;
	workspaces?: unknown;
	total?: unknown;
	count?: unknown;
	channel?: unknown;
	type?: unknown;
	workspace_unread?: unknown;
	total_unread?: unknown;
	state_event?: unknown;
	body?: unknown;
	comments?: unknown;
	remote_name?: unknown;
	target_branch?: unknown;
}

function record(value: unknown): WireRecord | null {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? (value as WireRecord)
		: null;
}

async function request(
	path: string,
	method = "GET",
	body?: unknown,
): Promise<unknown> {
	const response = await fetch(path, {
		method,
		credentials: "include",
		headers: {
			Accept: "application/json",
			...(body === undefined ? {} : { "Content-Type": "application/json" }),
		},
		...(body === undefined ? {} : { body: JSON.stringify(body) }),
	});
	if (!response.ok) {
		let message = `GitLab request failed (HTTP ${response.status})`;
		try {
			const payload = record(await response.json());
			const error = payload?.error;
			if (typeof error === "string") message = error;
		} catch {
			// Use the bounded status message rather than returning response text.
		}
		throw new Error(message);
	}
	if (response.status === 204) return null;
	return response.json();
}

function string(value: unknown, fallback = ""): string {
	return typeof value === "string" ? value : fallback;
}

function number(value: unknown): number {
	return typeof value === "number" && Number.isSafeInteger(value) ? value : 0;
}

function connection(value: unknown): GitLabConnection | null {
	const raw = record(value);
	if (!raw) return null;
	const allowed = new Set<GitLabEventCategory>([
		"assigned_issues",
		"authored_issues",
		"mentions_review_requests",
		"project_activity",
	]);
	const categories = Array.isArray(raw.event_categories)
		? raw.event_categories.filter(
				(item): item is GitLabEventCategory =>
					typeof item === "string" && allowed.has(item as GitLabEventCategory),
			)
		: [];
	return {
		host: string(raw.host),
		pathPrefix: string(raw.path_prefix),
		credentialId: string(raw.credential_id),
		revision: number(raw.revision),
		enabled: raw.enabled === true,
		eventCategories: categories,
		credentialReady: raw.credential_ready === true,
		connected: raw.connected === true,
		gitlabUsername:
			typeof raw.gitlab_username === "string" ? raw.gitlab_username : null,
	};
}

function mapping(value: unknown): GitLabProjectMapping | null {
	const raw = record(value);
	if (!raw) return null;
	return {
		workspacePath: string(raw.workspace_path),
		projectPath: string(raw.project_path),
		source: raw.source === "override" ? "override" : "remote",
	};
}

function event(value: unknown): GitLabInboxEvent | null {
	const raw = record(value);
	if (!raw) return null;
	const validCategories = new Set<GitLabEventCategory>([
		"assigned_issues",
		"authored_issues",
		"mentions_review_requests",
		"project_activity",
	]);
	const categories = Array.isArray(raw.categories)
		? raw.categories.filter(
				(item): item is GitLabEventCategory =>
					typeof item === "string" &&
					validCategories.has(item as GitLabEventCategory),
			)
		: [];
	return {
		id: string(raw.id),
		workspacePath: string(raw.workspace_path),
		projectPath: string(raw.project_path),
		category: string(raw.category, "project_activity") as GitLabEventCategory,
		categories,
		entityKind: raw.entity_kind === "merge_request" ? "merge_request" : "issue",
		entityIid: number(raw.entity_iid),
		relatedIssueIid:
			raw.related_issue_iid === null ? null : number(raw.related_issue_iid),
		discussionId:
			typeof raw.discussion_id === "string" ? raw.discussion_id : null,
		sessionId: typeof raw.session_id === "string" ? raw.session_id : null,
		title: string(raw.title),
		summary: string(raw.summary),
		url: typeof raw.url === "string" ? raw.url : null,
		occurredAt: string(raw.occurred_at),
		readAt: typeof raw.read_at === "string" ? raw.read_at : null,
	};
}

function ticket(value: unknown): GitLabTicket | null {
	const raw = record(value);
	if (!raw) return null;
	const statuses: readonly IssueStatus[] = [
		"open",
		"ready",
		"in_progress",
		"blocked",
		"closed",
	];
	const candidate = string(raw.status, "open");
	return {
		iid: number(raw.iid),
		id: string(raw.id, String(number(raw.iid))),
		title: string(raw.title),
		status: statuses.includes(candidate as IssueStatus)
			? (candidate as IssueStatus)
			: "open",
		priority: number(raw.priority) || 3,
		kind: string(raw.issue_type, "issue"),
		state: raw.state === "closed" ? "closed" : "opened",
		parentId: typeof raw.parent_id === "string" ? raw.parent_id : null,
		blockedBy: Array.isArray(raw.blocked_by)
			? raw.blocked_by.filter(
					(value): value is string => typeof value === "string",
				)
			: [],
		labels: Array.isArray(raw.labels)
			? raw.labels.filter((value): value is string => typeof value === "string")
			: [],
		description: string(raw.description),
		webUrl: typeof raw.web_url === "string" ? raw.web_url : null,
		updatedAt: string(raw.updated_at),
		statusConflict: raw.status_conflict === true,
		mergeRequestIid:
			typeof raw.merge_request_iid === "number" ? raw.merge_request_iid : null,
	};
}

function proposal(value: unknown): GitLabProposal {
	const raw = record(value) ?? {};
	const kind = string(raw.kind, "comment") as GitLabProposalDraft["kind"];
	const status = string(raw.status, "pending") as GitLabProposal["status"];
	return {
		id: string(raw.id),
		workspacePath: string(raw.workspace_path),
		projectPath: string(raw.project_path),
		issueIid: number(raw.issue_iid),
		sessionId: typeof raw.session_id === "string" ? raw.session_id : null,
		kind,
		payload: raw.payload,
		status,
		createdAt: string(raw.created_at),
		sessionStale: raw.session_stale === true,
	};
}

function wireProposalDraft(draft: GitLabProposalDraft): WireRecord {
	switch (draft.kind) {
		case "start_implementation":
			return {
				kind: draft.kind,
				...(draft.sessionId ? { session_id: draft.sessionId } : {}),
			};
		case "update_issue":
			return {
				kind: draft.kind,
				...(draft.sessionId ? { session_id: draft.sessionId } : {}),
				...(draft.title === undefined ? {} : { title: draft.title }),
				...(draft.description === undefined
					? {}
					: { description: draft.description }),
				...(draft.labels === undefined ? {} : { labels: draft.labels }),
				...(draft.stateEvent === undefined
					? {}
					: { state_event: draft.stateEvent }),
			};
		case "comment":
			return {
				kind: draft.kind,
				body: draft.body,
				...(draft.sessionId ? { session_id: draft.sessionId } : {}),
			};
		case "inline_review":
			return {
				kind: draft.kind,
				...(draft.sessionId ? { session_id: draft.sessionId } : {}),
				merge_request_iid: draft.mergeRequestIid,
				comments: draft.comments.map((comment) => ({
					path: comment.path,
					line: comment.line,
					side: comment.side,
					body: comment.body,
				})),
			};
		case "discussion_reply":
			return {
				kind: draft.kind,
				merge_request_iid: draft.mergeRequestIid,
				discussion_id: draft.discussionId,
				body: draft.body,
				...(draft.sessionId ? { session_id: draft.sessionId } : {}),
			};
	}
}

const query = (workspacePath: string) =>
	`?workspace_path=${encodeURIComponent(workspacePath)}`;

export function createLiveGitLabHost(openSocket?: () => MuxSocket): GitLabHost {
	return {
		async getConfig() {
			return connection(await request("/api/gitlab/config"));
		},
		async saveConfig(input: SaveGitLabConfigInput) {
			return connection(
				await request("/api/gitlab/config", "PUT", {
					instance_url: input.instanceUrl,
					enabled: input.enabled,
					event_categories: input.eventCategories,
				}),
			) as GitLabConnection;
		},
		async disconnect() {
			await request("/api/gitlab/config", "DELETE");
		},
		async testConnection(workspacePath) {
			const raw = record(
				await request("/api/gitlab/test", "POST", {
					workspace_path: workspacePath,
				}),
			);
			if (raw?.connected !== true || typeof raw.username !== "string") {
				throw new Error("GitLab connection test failed.");
			}
			return {
				username: raw.username,
				credentialRevision: string(raw.credential_revision),
			};
		},
		async getMapping(workspacePath) {
			return mapping(
				await request(`/api/gitlab/mapping${query(workspacePath)}`),
			);
		},
		async saveMapping(input: SaveGitLabMappingInput) {
			return mapping(
				await request("/api/gitlab/mapping", "PUT", {
					workspace_path: input.workspacePath,
					...(input.remoteUrl ? { remote_url: input.remoteUrl } : {}),
					...(input.projectPathOverride
						? { project_path_override: input.projectPathOverride }
						: {}),
				}),
			) as GitLabProjectMapping;
		},
		async removeMapping(workspacePath) {
			await request(`/api/gitlab/mapping${query(workspacePath)}`, "DELETE");
		},
		async inbox(workspacePath, poll = false) {
			const value = poll
				? await request("/api/gitlab/inbox/poll", "POST", {
						workspace_path: workspacePath,
					})
				: await request(`/api/gitlab/inbox${query(workspacePath)}`);
			return Array.isArray(value)
				? value
						.map(event)
						.filter((item): item is GitLabInboxEvent => item !== null)
				: [];
		},
		async listIssues(workspacePath): Promise<GitLabTicketList> {
			const raw = record(
				await request(`/api/gitlab/issues${query(workspacePath)}`),
			);
			return {
				issues: Array.isArray(raw?.issues)
					? raw.issues
							.map(ticket)
							.filter((item): item is GitLabTicket => item !== null)
					: [],
				truncated: raw?.truncated === true,
			};
		},
		async selectIssue(workspacePath, sessionId, issueIid) {
			await request("/api/gitlab/session-selection", "POST", {
				workspace_path: workspacePath,
				session_id: sessionId,
				issue_iid: issueIid,
			});
		},
		async markRead(eventId) {
			await request(
				`/api/gitlab/inbox/${encodeURIComponent(eventId)}/read`,
				"POST",
				{},
			);
		},
		async unreadCounts() {
			const raw = record(await request("/api/gitlab/inbox/counts"));
			const byWorkspacePath: Record<string, number> = {};
			if (Array.isArray(raw?.workspaces)) {
				for (const row of raw.workspaces) {
					const item = record(row);
					if (!item) continue;
					const path = string(item.workspace_path);
					if (path) byWorkspacePath[path] = number(item.count);
				}
			}
			return { total: number(raw?.total), byWorkspacePath };
		},
		watchInboxUpdates(listener) {
			if (!openSocket) return () => undefined;
			const socket = openSocket();
			socket.onmessage = (event) => {
				let raw: WireRecord | null = null;
				try {
					raw = record(JSON.parse(String(event.data)));
				} catch {
					return;
				}
				if (raw?.channel !== "system" || raw.type !== "gitlab_inbox_updated")
					return;
				const workspacePath = string(raw.workspace_path);
				const count = number(raw.workspace_unread);
				listener({
					total: number(raw.total_unread),
					byWorkspacePath: workspacePath ? { [workspacePath]: count } : {},
				});
			};
			return () => socket.close();
		},
		async proposals(workspacePath) {
			const value = await request(
				`/api/gitlab/proposals${query(workspacePath)}`,
			);
			return Array.isArray(value) ? value.map(proposal) : [];
		},
		async createProposal(input: CreateGitLabProposalInput) {
			return proposal(
				await request("/api/gitlab/proposals", "POST", {
					workspace_path: input.workspacePath,
					issue_iid: input.issueIid,
					proposal: wireProposalDraft(input.proposal),
				}),
			);
		},
		async approveProposal(id, overwriteStale = false) {
			return proposal(
				await request(
					`/api/gitlab/proposals/${encodeURIComponent(id)}/approve`,
					"POST",
					{ overwrite_stale: overwriteStale },
				),
			);
		},
		async rebindProposal(id, sessionId) {
			return proposal(
				await request(
					`/api/gitlab/proposals/${encodeURIComponent(id)}/rebind`,
					"POST",
					{ session_id: sessionId },
				),
			);
		},
		async rejectProposal(id) {
			await request(
				`/api/gitlab/proposals/${encodeURIComponent(id)}/reject`,
				"POST",
				{},
			);
		},
		async createOrUpdateDraft(input: CreateGitLabDraftInput) {
			return request("/api/gitlab/merge-requests/draft", "POST", {
				workspace_path: input.workspacePath,
				issue_iid: input.issueIid,
				remote_name: input.remoteName,
				...(input.targetBranch ? { target_branch: input.targetBranch } : {}),
				title: input.title,
				description: input.description,
				confirm_redraft_ready: input.confirmRedraftReady,
			});
		},
		async markMergeRequestReady(input: MarkGitLabMergeRequestReadyInput) {
			return request("/api/gitlab/merge-requests/ready", "POST", {
				workspace_path: input.workspacePath,
				issue_iid: input.issueIid,
				confirmed: input.confirmed,
			});
		},
	};
}
