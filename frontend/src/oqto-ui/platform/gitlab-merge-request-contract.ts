export interface CreateGitLabDraftInput {
	readonly workspacePath: string;
	readonly issueIid: number;
	readonly remoteName: string;
	readonly targetBranch?: string;
	readonly title: string;
	readonly description: string;
	readonly confirmRedraftReady: true;
}

export interface MarkGitLabMergeRequestReadyInput {
	readonly workspacePath: string;
	readonly issueIid: number;
	readonly confirmed: true;
}

export interface GitLabMergeRequestHost {
	createOrUpdateDraft(input: CreateGitLabDraftInput): Promise<unknown>;
	markMergeRequestReady(
		input: MarkGitLabMergeRequestReadyInput,
	): Promise<unknown>;
}
