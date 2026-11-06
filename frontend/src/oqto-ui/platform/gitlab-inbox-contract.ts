import type {
	GitLabInboxEvent,
	GitLabTicketList,
	GitLabUnreadCounts,
} from "./gitlab-types";

export interface GitLabInboxHost {
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
}
