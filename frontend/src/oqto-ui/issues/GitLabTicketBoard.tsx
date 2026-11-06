import { useTranslation } from "react-i18next";
import { GitLabTicketForms } from "./GitLabTicketForms";
import { GITLAB_TICKET_STATUSES } from "./gitlab-inbox-constants";
import { ticketAsEvent } from "./gitlab-inbox-utils";
import type { GitLabInboxController } from "./useGitLabInboxController";

interface GitLabTicketBoardProps {
	readonly controller: GitLabInboxController;
}

export function GitLabTicketBoard({ controller }: GitLabTicketBoardProps) {
	const { t } = useTranslation();
	const tickets = controller.tickets.filter(
		(ticket) => controller.showClosedIssues || ticket.status !== "closed",
	);
	if (tickets.length === 0) {
		return (
			<p className="wb-gitlab__tickets-empty">
				{t("oqtoUi.gitlab.noAssignedIssues")}
			</p>
		);
	}

	return (
		<section
			className="wb-gitlab__tickets"
			aria-label={t("oqtoUi.gitlab.issuesLabel")}
		>
			{controller.issuesTruncated && (
				<output>{t("oqtoUi.gitlab.issuesTruncated")}</output>
			)}
			{GITLAB_TICKET_STATUSES.map((status) => {
				const group = tickets.filter((ticket) => ticket.status === status);
				if (group.length === 0) return null;
				return (
					<section key={status} className="wb-issues__group">
						<h3>{t(`oqtoUi.issues.status.${status}`)}</h3>
						<ul>
							{group.map((ticket) => {
								const event = ticketAsEvent(
									ticket,
									controller.projectOverride,
									controller.workspacePath,
								);
								return (
									<li key={ticket.iid} data-status={ticket.status}>
										<span className="wb-issues__id">#{ticket.iid}</span>
										<span className="wb-issues__title">{ticket.title}</span>
										{ticket.statusConflict && (
											<small className="wb-gitlab__error">
												{t("oqtoUi.gitlab.statusConflict")}
											</small>
										)}
										<div className="wb-gitlab__actions">
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
												disabled={
													controller.busy ||
													ticket.state === "closed" ||
													ticket.statusConflict
												}
												onClick={() =>
													void controller.propose(ticket.iid, {
														kind: "start_implementation",
														sessionId: controller.sessionId,
													})
												}
											>
												{t("oqtoUi.gitlab.proposeStart")}
											</button>
											<button
												type="button"
												onClick={() => {
													controller.setCommentIssue(ticket.iid);
													controller.setEditIssue(null);
												}}
											>
												{t("oqtoUi.gitlab.proposeComment")}
											</button>
											<button
												type="button"
												onClick={() => {
													controller.setEditIssue(ticket.iid);
													controller.setCommentIssue(null);
													controller.setEditTitle(ticket.title);
													controller.setEditDescription(ticket.description);
												}}
											>
												{t("oqtoUi.gitlab.proposeEdit")}
											</button>
											<select
												aria-label={t("oqtoUi.gitlab.proposeStatus", {
													iid: ticket.iid,
												})}
												value={ticket.status}
												onChange={(changeEvent) =>
													void controller.proposeTicketStatus(
														ticket,
														changeEvent.target.value as typeof ticket.status,
													)
												}
												disabled={controller.busy || ticket.statusConflict}
											>
												{GITLAB_TICKET_STATUSES.map((value) => (
													<option key={value} value={value}>
														{t(`oqtoUi.issues.status.${value}`)}
													</option>
												))}
											</select>
											{ticket.mergeRequestIid !== null &&
												controller.appliedStarts.has(ticket.iid) && (
													<>
														<button
															type="button"
															onClick={() => controller.setMrIssue(ticket.iid)}
														>
															{t("oqtoUi.gitlab.draftUpdateMr")}
														</button>
														<button
															type="button"
															onClick={() => void controller.ready(event)}
														>
															{t("oqtoUi.gitlab.markMrReady")}
														</button>
														<button
															type="button"
															onClick={() => {
																controller.setReviewIssue(ticket.iid);
																controller.setReviewMrIid(
																	String(ticket.mergeRequestIid),
																);
															}}
														>
															{t("oqtoUi.gitlab.proposeInlineReview")}
														</button>
													</>
												)}
										</div>
										{ticket.labels.length > 0 && (
											<small>{ticket.labels.join(", ")}</small>
										)}
										<GitLabTicketForms
											controller={controller}
											ticket={ticket}
										/>
									</li>
								);
							})}
						</ul>
					</section>
				);
			})}
		</section>
	);
}
