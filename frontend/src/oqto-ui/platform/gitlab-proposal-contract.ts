import type { GitLabProposal, GitLabProposalDraft } from "./gitlab-types";

export interface CreateGitLabProposalInput {
	readonly workspacePath: string;
	readonly issueIid: number;
	readonly proposal: GitLabProposalDraft;
}

export interface GitLabProposalHost {
	proposals(workspacePath: string): Promise<readonly GitLabProposal[]>;
	createProposal(input: CreateGitLabProposalInput): Promise<GitLabProposal>;
	approveProposal(
		id: string,
		overwriteStale?: boolean,
	): Promise<GitLabProposal>;
	rebindProposal(id: string, sessionId: string): Promise<GitLabProposal>;
	rejectProposal(id: string): Promise<void>;
}
