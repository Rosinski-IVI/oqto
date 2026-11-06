import type { GitHost } from "../platform/git-contract";
import type { GitLabHost, GitLabInboxEvent } from "../platform/gitlab-contract";
import { GitLabActivityFeed } from "./GitLabActivityFeed";
import { GitLabConnectionPanel } from "./GitLabConnectionPanel";
import { GitLabProposalList } from "./GitLabProposalList";
import { GitLabTicketBoard } from "./GitLabTicketBoard";
import { useGitLabInboxController } from "./useGitLabInboxController";

interface GitLabInboxPaneProps {
	readonly host: GitLabHost;
	readonly gitHost?: GitHost;
	readonly workspacePath: string;
	readonly sessionId: string;
	readonly onPlan: (event: GitLabInboxEvent) => void;
	readonly onStartImplementation: (
		event: GitLabInboxEvent,
		sessionId: string | null,
	) => void;
	readonly onAddressComment: (event: GitLabInboxEvent) => void;
}

export function GitLabInboxPane(props: GitLabInboxPaneProps) {
	const controller = useGitLabInboxController(props);
	if (!controller.connection) {
		return <GitLabConnectionPanel controller={controller} />;
	}

	return (
		<div className="wb-gitlab">
			<GitLabConnectionPanel controller={controller} />
			{controller.error && (
				<p role="alert" className="wb-gitlab__error">
					{controller.error}
				</p>
			)}
			<GitLabProposalList controller={controller} />
			{controller.projectOverride &&
				!controller.editingMapping &&
				controller.gitlabView === "issues" && (
					<GitLabTicketBoard controller={controller} />
				)}
			{controller.projectOverride &&
				!controller.editingMapping &&
				controller.gitlabView === "inbox" && (
					<GitLabActivityFeed controller={controller} />
				)}
		</div>
	);
}
