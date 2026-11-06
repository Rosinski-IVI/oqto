import type { GitLabHost } from "@/src/oqto-ui/platform/gitlab-contract";
import { useEffect } from "react";

interface GitLabInboxWatchOptions {
	host: GitLabHost;
	connected: boolean;
	projectPath: string;
	workspacePath: string;
	refresh: () => Promise<void>;
	onError: (message: string) => void;
}

/** Subscribes to workspace updates; the host also polls as a fallback. */
export function useGitLabInboxWatch({
	host,
	connected,
	projectPath,
	workspacePath,
	refresh,
	onError,
}: GitLabInboxWatchOptions): void {
	// useeffect-guardrail: allow - manage the workspace GitLab event subscription lifecycle
	useEffect(() => {
		if (!connected || !projectPath || !host.watchInboxUpdates) return;
		return host.watchInboxUpdates((counts) => {
			if (!(workspacePath in counts.byWorkspacePath)) return;
			void refresh().catch((reason: unknown) =>
				onError(reason instanceof Error ? reason.message : String(reason)),
			);
		});
	}, [connected, host, projectPath, refresh, workspacePath, onError]);
}
