import { useTranslation } from "react-i18next";
import { GitLabActivityForms } from "./GitLabActivityForms";
import type { GitLabInboxController } from "./useGitLabInboxController";

interface GitLabActivityFeedProps {
	readonly controller: GitLabInboxController;
}

export function GitLabActivityFeed({ controller }: GitLabActivityFeedProps) {
	const { t } = useTranslation();
	if (controller.events.length === 0) {
		return <p>{t("oqtoUi.gitlab.noEvents")}</p>;
	}
	return (
		<div className="wb-gitlab__events">
			{controller.events.map((event) => {
				const issueIid = event.relatedIssueIid ?? event.entityIid;
				const categoryKeys =
					event.categories.length > 0 ? event.categories : [event.category];
				const hasApprovedStart = controller.appliedStarts.has(issueIid);
				return (
					<article className="wb-gitlab__event" key={event.id}>
						<div className="wb-gitlab__event-head">
							<strong>
								{t("oqtoUi.gitlab.eventTitle", {
									kind: t(`oqtoUi.gitlab.entityKinds.${event.entityKind}`),
									iid: event.entityIid,
									title: event.title,
								})}
							</strong>
							<small>
								{categoryKeys
									.map((category) => t(`oqtoUi.gitlab.categories.${category}`))
									.join(" · ")}
							</small>
						</div>
						{event.summary && <p>{event.summary}</p>}
						<div className="wb-gitlab__actions">
							{event.url && (
								<a
									href={event.url}
									target="_blank"
									rel="noreferrer"
									onClick={() => void controller.markEventRead(event)}
								>
									{t("oqtoUi.gitlab.openInGitLab")}
								</a>
							)}
							{event.entityKind === "merge_request" &&
								event.relatedIssueIid !== null && (
									<button
										type="button"
										onClick={() =>
											void controller.routeEventToSession(event, () =>
												controller.onAddressComment(event),
											)
										}
									>
										{t("oqtoUi.gitlab.addressComment")}
									</button>
								)}
							{event.entityKind === "merge_request" &&
								event.relatedIssueIid !== null &&
								event.discussionId && (
									<button
										type="button"
										onClick={() =>
											void controller.markEventRead(event).then(() => {
												controller.setReplyEventId(event.id);
												controller.setReplyBody("");
											})
										}
									>
										{t("oqtoUi.gitlab.proposeThreadReplyButton")}
									</button>
								)}
							{event.entityKind === "issue" && (
								<>
									<button
										type="button"
										onClick={() =>
											void controller.routeEventToSession(event, () =>
												controller.onPlan(event),
											)
										}
									>
										{t("oqtoUi.gitlab.planInSession")}
									</button>
									<button
										type="button"
										disabled={controller.busy}
										onClick={() =>
											void controller.propose(event.entityIid, {
												kind: "start_implementation",
												sessionId: controller.sessionId,
											})
										}
									>
										{t("oqtoUi.gitlab.proposeImplementationStart")}
									</button>
									<button
										type="button"
										onClick={() => {
											controller.setCommentIssue(event.entityIid);
											controller.setEditIssue(null);
										}}
									>
										{t("oqtoUi.gitlab.proposeComment")}
									</button>
									<button
										type="button"
										onClick={() => {
											controller.setEditIssue(event.entityIid);
											controller.setCommentIssue(null);
											controller.setEditTitle(event.title);
										}}
									>
										{t("oqtoUi.gitlab.proposeIssueEdit")}
									</button>
								</>
							)}
							{hasApprovedStart && (
								<>
									<button
										type="button"
										onClick={() => controller.setMrIssue(issueIid)}
									>
										{event.entityKind === "issue"
											? t("oqtoUi.gitlab.draftMr")
											: t("oqtoUi.gitlab.updateDraftMr")}
									</button>
									{event.entityKind === "issue" && (
										<button
											type="button"
											onClick={() => controller.setReviewIssue(event.entityIid)}
										>
											{t("oqtoUi.gitlab.proposeInlineReview")}
										</button>
									)}
								</>
							)}
						</div>
						<GitLabActivityForms controller={controller} event={event} />
					</article>
				);
			})}
		</div>
	);
}
