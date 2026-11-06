import { useTranslation } from "react-i18next";
import { proposedIssueChanges } from "./gitlab-inbox-utils";
import type { GitLabInboxController } from "./useGitLabInboxController";

interface GitLabProposalListProps {
	readonly controller: GitLabInboxController;
}

export function GitLabProposalList({ controller }: GitLabProposalListProps) {
	const { t } = useTranslation();
	const visibleProposals = controller.proposals.filter(
		(proposal) =>
			proposal.status === "pending" ||
			(proposal.status === "approved" && proposal.kind === "inline_review"),
	);
	if (visibleProposals.length === 0) return null;

	return (
		<section
			className="wb-gitlab__proposals"
			aria-label={t("oqtoUi.gitlab.approvalProposals")}
		>
			<h3>{t("oqtoUi.gitlab.approvalRequired")}</h3>
			{visibleProposals.map((proposal) => {
				const changes = proposedIssueChanges(proposal);
				const summary = changes?.length
					? t("oqtoUi.gitlab.proposedChanges", {
							changes: changes.join(" · "),
						})
					: t("oqtoUi.gitlab.genericProposal");
				return (
					<div className="wb-gitlab__proposal" key={proposal.id}>
						<div>
							<strong>
								{t("oqtoUi.gitlab.proposalForIssue", {
									kind: t(`oqtoUi.gitlab.proposalKinds.${proposal.kind}`),
									iid: proposal.issueIid,
								})}
							</strong>
							<p>{summary}</p>
							{proposal.sessionStale && (
								<p role="alert">{t("oqtoUi.gitlab.staleSession")}</p>
							)}
						</div>
						{proposal.sessionStale && (
							<button
								type="button"
								disabled={controller.busy || !controller.sessionId}
								onClick={() => void controller.rebindProposal(proposal)}
							>
								{t("oqtoUi.gitlab.rebindSession")}
							</button>
						)}
						{!proposal.sessionStale && (
							<button
								type="button"
								disabled={controller.busy}
								onClick={() => void controller.decideProposal(proposal, true)}
							>
								{proposal.status === "approved"
									? t("oqtoUi.gitlab.resumeReview")
									: t("oqtoUi.gitlab.approve")}
							</button>
						)}
						{proposal.status === "pending" && (
							<button
								type="button"
								disabled={controller.busy}
								onClick={() => void controller.decideProposal(proposal, false)}
							>
								{t("oqtoUi.gitlab.reject")}
							</button>
						)}
					</div>
				);
			})}
		</section>
	);
}
