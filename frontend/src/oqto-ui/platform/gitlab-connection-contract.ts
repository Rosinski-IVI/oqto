import type { GitLabConnection, GitLabEventCategory } from "./gitlab-types";

export interface SaveGitLabConfigInput {
	readonly instanceUrl: string;
	readonly enabled: boolean;
	readonly eventCategories: readonly GitLabEventCategory[];
}

export interface GitLabConnectionTest {
	readonly username: string;
	readonly credentialRevision: string;
}

export interface GitLabConnectionHost {
	getConfig(): Promise<GitLabConnection | null>;
	saveConfig(input: SaveGitLabConfigInput): Promise<GitLabConnection>;
	testConnection(workspacePath: string): Promise<GitLabConnectionTest>;
	disconnect(): Promise<void>;
}
