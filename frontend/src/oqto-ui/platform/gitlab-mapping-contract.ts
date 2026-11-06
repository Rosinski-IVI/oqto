import type { GitLabProjectMapping } from "./gitlab-types";

export interface SaveGitLabMappingInput {
	readonly workspacePath: string;
	readonly remoteUrl?: string;
	readonly projectPathOverride?: string;
}

export interface GitLabMappingHost {
	getMapping(workspacePath: string): Promise<GitLabProjectMapping | null>;
	saveMapping(input: SaveGitLabMappingInput): Promise<GitLabProjectMapping>;
	removeMapping(workspacePath: string): Promise<void>;
}
