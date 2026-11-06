import type { GitRemote } from "../platform/git-contract";
import type {
	GitLabInboxEvent,
	GitLabProposal,
	GitLabTicket,
} from "../platform/gitlab-contract";

interface IssueFields {
	title?: unknown;
	description?: unknown;
	labels?: unknown;
	state?: unknown;
}

interface IssueProposalPayload extends IssueFields {
	original?: IssueFields | null;
	state_event?: unknown;
}

export function remoteHost(remoteUrl: string): string | null {
	if (remoteUrl.startsWith("git@")) {
		const rest = remoteUrl.slice("git@".length);
		const host = rest.split(":", 1)[0];
		return host?.toLowerCase() || null;
	}
	try {
		return new URL(remoteUrl).hostname.toLowerCase();
	} catch {
		return null;
	}
}

export function preferredRemote(
	remotes: readonly GitRemote[],
	host: string,
): string {
	const matching = remotes.filter((remote) => remoteHost(remote.url) === host);
	return matching.find((remote) => remote.name === "origin")?.url ?? "";
}

export function preferredRemoteEntry(
	remotes: readonly GitRemote[],
	host: string,
	pathPrefix: string,
	projectPath?: string,
): GitRemote | undefined {
	const matching = remotes.filter(
		(remote) =>
			remoteHost(remote.url) === host &&
			(!projectPath || remoteProject(remote.url, pathPrefix) === projectPath),
	);
	return matching.find((remote) => remote.name === "origin");
}

export function remoteProject(
	remoteUrl: string,
	pathPrefix: string,
): string | null {
	let rawPath: string;
	let isScp = false;
	if (remoteUrl.startsWith("git@")) {
		rawPath = remoteUrl.slice("git@".length).split(":").slice(1).join(":");
		isScp = true;
	} else {
		try {
			const url = new URL(remoteUrl);
			rawPath = url.pathname.replace(/^\//, "");
			isScp = url.protocol === "ssh:";
		} catch {
			return null;
		}
	}
	const prefix = pathPrefix.replace(/^\/+|\/+$/g, "");
	if (prefix) {
		if (rawPath.startsWith(`${prefix}/`))
			rawPath = rawPath.slice(prefix.length + 1);
		else if (!isScp) return null;
	}
	return rawPath.replace(/\.git$/i, "") || null;
}

export function ticketAsEvent(
	ticket: GitLabTicket,
	projectPath: string,
	workspacePath: string,
): GitLabInboxEvent {
	return {
		id: `issue-${ticket.iid}`,
		workspacePath,
		projectPath,
		category: "assigned_issues",
		categories: ["assigned_issues"],
		entityKind: "issue",
		entityIid: ticket.iid,
		relatedIssueIid: null,
		discussionId: null,
		sessionId: null,
		title: ticket.title,
		summary: ticket.description,
		url: ticket.webUrl,
		occurredAt: ticket.updatedAt,
		readAt: null,
	};
}

function proposalPayload(value: unknown): IssueProposalPayload | null {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? (value as IssueProposalPayload)
		: null;
}

function displayValue(value: unknown): string {
	const text = Array.isArray(value)
		? value.join(", ")
		: String(value ?? "(empty)");
	return text.length > 120 ? `${text.slice(0, 117)}…` : text;
}

export function proposedIssueChanges(
	proposal: GitLabProposal,
): readonly string[] | null {
	if (proposal.kind !== "update_issue") return null;
	const payload = proposalPayload(proposal.payload);
	if (!payload) return null;
	const original = proposalPayload(payload.original);
	const changes: string[] = [];
	for (const key of ["title", "description", "labels"] as const) {
		const proposed = payload[key];
		if (proposed === undefined || proposed === null) continue;
		const before = original?.[key];
		if (JSON.stringify(before) === JSON.stringify(proposed)) continue;
		changes.push(`${key}: ${displayValue(before)} → ${displayValue(proposed)}`);
	}
	if (payload.state_event === "close" || payload.state_event === "reopen") {
		changes.push(
			`state: ${displayValue(original?.state)} → ${payload.state_event === "close" ? "closed" : "opened"}`,
		);
	}
	return changes;
}
