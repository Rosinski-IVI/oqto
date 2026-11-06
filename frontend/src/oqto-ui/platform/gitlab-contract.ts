import type { Issue } from "./issues-contract";

export type GitLabEventCategory =
	| "assigned_issues"
	| "authored_issues"
	| "mentions_review_requests"
	| "project_activity";

export interface GitLabConnection {
	readonly host: string;
	readonly pathPrefix: string;
	readonly credentialId: string;
	readonly revision: number;
	readonly enabled: boolean;
	readonly eventCategories: readonly GitLabEventCategory[];
	readonly credentialReady: boolean;
	readonly connected: boolean;
	readonly gitlabUsername: string | null;
}

export interface GitLabProjectMapping {
	readonly workspacePath: string;
	readonly projectPath: string;
	readonly source: "remote" | "override";
}

export interface GitLabTicket extends Issue {
	readonly iid: number;
	readonly description: string;
	readonly state: "opened" | "closed";
	readonly webUrl: string | null;
	readonly updatedAt: string;
	readonly statusConflict: boolean;
	readonly mergeRequestIid: number | null;
}

export interface GitLabTicketList {
	readonly issues: readonly GitLabTicket[];
	readonly truncated: boolean;
}

export interface GitLabInboxEvent {
	readonly id: string;
	readonly workspacePath: string;
	readonly projectPath: string;
	readonly category: GitLabEventCategory;
	readonly categories: readonly GitLabEventCategory[];
	readonly entityKind: "issue" | "merge_request";
	readonly entityIid: number;
	readonly relatedIssueIid: number | null;
	readonly discussionId: string | null;
	readonly sessionId: string | null;
	readonly title: string;
	readonly summary: string;
	readonly url: string | null;
	readonly occurredAt: string;
	readonly readAt: string | null;
}

export type GitLabProposalDraft =
	| { kind: "start_implementation"; sessionId?: string }
	| {
			kind: "update_issue";
			sessionId?: string;
			title?: string;
			description?: string;
			labels?: readonly string[];
			stateEvent?: "close" | "reopen";
	  }
	| { kind: "comment"; sessionId?: string; body: string }
	| {
			kind: "inline_review";
			sessionId?: string;
			mergeRequestIid: number;
			comments: readonly {
				path: string;
				line: number;
				side: "old" | "new";
				body: string;
			}[];
	  }
	| {
			kind: "discussion_reply";
			sessionId?: string;
			mergeRequestIid: number;
			discussionId: string;
			body: string;
	  };

export interface GitLabProposal {
	readonly id: string;
	readonly workspacePath: string;
	readonly projectPath: string;
	readonly issueIid: number;
	readonly sessionId: string | null;
	readonly kind: GitLabProposalDraft["kind"];
	readonly payload: unknown;
	readonly status:
		| "pending"
		| "approved"
		| "rejected"
		| "applied"
		| "failed"
		| "expired";
	readonly createdAt: string;
	readonly sessionStale: boolean;
}

export interface GitLabUnreadCounts {
	readonly total: number;
	readonly byWorkspacePath: Readonly<Record<string, number>>;
}

export interface GitLabHost {
	getConfig(): Promise<GitLabConnection | null>;
	saveConfig(input: {
		instanceUrl: string;
		enabled: boolean;
		eventCategories: readonly GitLabEventCategory[];
	}): Promise<GitLabConnection>;
	testConnection(
		workspacePath: string,
	): Promise<{ username: string; credentialRevision: string }>;
	disconnect(): Promise<void>;
	getMapping(workspacePath: string): Promise<GitLabProjectMapping | null>;
	saveMapping(input: {
		workspacePath: string;
		remoteUrl?: string;
		projectPathOverride?: string;
	}): Promise<GitLabProjectMapping>;
	removeMapping(workspacePath: string): Promise<void>;
	inbox(
		workspacePath: string,
		poll?: boolean,
	): Promise<readonly GitLabInboxEvent[]>;
	listIssues(workspacePath: string): Promise<GitLabTicketList>;
	selectIssue(
		workspacePath: string,
		sessionId: string,
		issueIid: number,
	): Promise<void>;
	markRead(eventId: string): Promise<void>;
	unreadCounts(): Promise<GitLabUnreadCounts>;
	watchInboxUpdates?(
		listener: (counts: GitLabUnreadCounts) => void,
	): () => void;
	proposals(workspacePath: string): Promise<readonly GitLabProposal[]>;
	createProposal(input: {
		workspacePath: string;
		issueIid: number;
		proposal: GitLabProposalDraft;
	}): Promise<GitLabProposal>;
	approveProposal(
		id: string,
		overwriteStale?: boolean,
	): Promise<GitLabProposal>;
	rebindProposal(id: string, sessionId: string): Promise<GitLabProposal>;
	rejectProposal(id: string): Promise<void>;
	createOrUpdateDraft(input: {
		workspacePath: string;
		issueIid: number;
		remoteName: string;
		targetBranch?: string;
		title: string;
		description: string;
		confirmRedraftReady: true;
	}): Promise<unknown>;
	markMergeRequestReady(input: {
		workspacePath: string;
		issueIid: number;
		confirmed: true;
	}): Promise<unknown>;
}
