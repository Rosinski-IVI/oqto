import type {
	GitLabHost,
	GitLabUnreadCounts,
} from "@/src/oqto-ui/platform/gitlab-contract";
import { useEffect, useState } from "react";

const EMPTY_COUNTS: GitLabUnreadCounts = { total: 0, byWorkspacePath: {} };

/** Owns the live subscription and fallback polling for the GitLab unread badge. */
export function useGitLabUnreadCounts(
	host: GitLabHost | undefined,
): GitLabUnreadCounts {
	const [counts, setCounts] = useState(EMPTY_COUNTS);

	// useeffect-guardrail: allow - own the live unread subscription and polling fallback
	useEffect(() => {
		if (!host) {
			setCounts(EMPTY_COUNTS);
			return;
		}
		let active = true;
		const load = () => {
			void host
				.unreadCounts()
				.then((next) => {
					if (active) setCounts(next);
				})
				.catch(() => undefined);
		};
		load();
		const stopWatching = host.watchInboxUpdates?.((next) => {
			if (!active) return;
			setCounts((current) => ({
				total: next.total,
				byWorkspacePath: {
					...current.byWorkspacePath,
					...next.byWorkspacePath,
				},
			}));
		});
		const timer = window.setInterval(load, 60_000);
		return () => {
			active = false;
			window.clearInterval(timer);
			stopWatching?.();
		};
	}, [host]);

	return counts;
}
