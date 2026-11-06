import { useTranslation } from "react-i18next";
import type { GitLabInboxEvent } from "../platform/gitlab-contract";
import type { GitLabInboxController } from "./useGitLabInboxController";

interface GitLabActivityFormsProps {
	readonly controller: GitLabInboxController;
	readonly event: GitLabInboxEvent;
}

export function GitLabActivityForms({
	controller,
	event,
}: GitLabActivityFormsProps) {
	const { t } = useTranslation();
	return (
		<>
			{controller.commentIssue === event.entityIid && (
				<form
					className="wb-gitlab__form"
					onSubmit={(submitEvent) => {
						submitEvent.preventDefault();
						void controller.propose(event.entityIid, {
							kind: "comment",
							sessionId: controller.sessionId,
							body: controller.comment,
						});
					}}
				>
					<label>
						{t("oqtoUi.gitlab.comment")}
						<textarea
							value={controller.comment}
							onChange={(changeEvent) =>
								controller.setComment(changeEvent.target.value)
							}
							required
						/>
					</label>
					<button type="submit" disabled={controller.busy}>
						{t("oqtoUi.gitlab.submitForApproval")}
					</button>
				</form>
			)}
			{controller.editIssue === event.entityIid && (
				<form
					className="wb-gitlab__form"
					onSubmit={(submitEvent) => {
						submitEvent.preventDefault();
						void controller.propose(event.entityIid, {
							kind: "update_issue",
							sessionId: controller.sessionId,
							title: controller.editTitle,
							...(controller.editDescription.trim()
								? { description: controller.editDescription }
								: {}),
						});
					}}
				>
					<label>
						{t("oqtoUi.gitlab.proposedTitle")}
						<input
							value={controller.editTitle}
							onChange={(changeEvent) =>
								controller.setEditTitle(changeEvent.target.value)
							}
							required
						/>
					</label>
					<label>
						{t("oqtoUi.gitlab.proposedDescription")}
						<textarea
							value={controller.editDescription}
							onChange={(changeEvent) =>
								controller.setEditDescription(changeEvent.target.value)
							}
						/>
					</label>
					<button type="submit" disabled={controller.busy}>
						{t("oqtoUi.gitlab.submitEditForApproval")}
					</button>
				</form>
			)}
			{controller.mrIssue === (event.relatedIssueIid ?? event.entityIid) && (
				<form
					className="wb-gitlab__form"
					onSubmit={(submitEvent) => {
						submitEvent.preventDefault();
						void controller.makeDraft(event);
					}}
				>
					<p>
						{t("oqtoUi.gitlab.currentBranch")}:{" "}
						<code>
							{controller.currentBranch || t("oqtoUi.gitlab.unavailable")}
						</code>
					</p>
					<label>
						{t("oqtoUi.gitlab.gitRemote")}
						<select
							value={controller.draftRemoteName}
							onChange={(changeEvent) =>
								controller.setDraftRemoteName(changeEvent.target.value)
							}
							required
						>
							<option value="">
								{t("oqtoUi.gitlab.selectMatchingRemote")}
							</option>
							{controller.matchingRemotes.map((remote) => (
								<option key={remote.name} value={remote.name}>
									{remote.name}: {remote.url}
								</option>
							))}
						</select>
					</label>
					<label>
						{t("oqtoUi.gitlab.targetBranchOverride")}
						<input
							value={controller.targetBranch}
							onChange={(changeEvent) =>
								controller.setTargetBranch(changeEvent.target.value)
							}
							placeholder={t("oqtoUi.gitlab.targetBranchPlaceholder")}
						/>
					</label>
					<button type="submit" disabled={controller.busy}>
						{t("oqtoUi.gitlab.createUpdateDraft")}
					</button>
					<button
						type="button"
						disabled={controller.busy}
						onClick={() => void controller.ready(event)}
					>
						{t("oqtoUi.gitlab.markDraftReady")}
					</button>
				</form>
			)}
			{controller.reviewIssue === event.entityIid &&
				event.entityKind === "issue" && (
					<form
						className="wb-gitlab__form"
						onSubmit={(submitEvent) => {
							submitEvent.preventDefault();
							void controller.propose(event.entityIid, {
								kind: "inline_review",
								sessionId: controller.sessionId,
								mergeRequestIid: Number(controller.reviewMrIid),
								comments: [
									{
										path: controller.reviewPath,
										line: Number(controller.reviewLine),
										side: controller.reviewSide,
										body: controller.reviewBody,
									},
								],
							});
						}}
					>
						<label>
							{t("oqtoUi.gitlab.mergeRequestIid")}
							<input
								type="number"
								min="1"
								value={controller.reviewMrIid}
								onChange={(changeEvent) =>
									controller.setReviewMrIid(changeEvent.target.value)
								}
								required
							/>
						</label>
						<label>
							{t("oqtoUi.gitlab.changedFilePath")}
							<input
								value={controller.reviewPath}
								onChange={(changeEvent) =>
									controller.setReviewPath(changeEvent.target.value)
								}
								required
							/>
						</label>
						<label>
							{t("oqtoUi.gitlab.diffSide")}
							<select
								value={controller.reviewSide}
								onChange={(changeEvent) =>
									controller.setReviewSide(
										changeEvent.target.value as "old" | "new",
									)
								}
							>
								<option value="new">{t("oqtoUi.gitlab.newSide")}</option>
								<option value="old">{t("oqtoUi.gitlab.oldSide")}</option>
							</select>
						</label>
						<label>
							{t("oqtoUi.gitlab.diffLineNumber")}
							<input
								type="number"
								min="1"
								value={controller.reviewLine}
								onChange={(changeEvent) =>
									controller.setReviewLine(changeEvent.target.value)
								}
								required
							/>
						</label>
						<label>
							{t("oqtoUi.gitlab.reviewComment")}
							<textarea
								value={controller.reviewBody}
								onChange={(changeEvent) =>
									controller.setReviewBody(changeEvent.target.value)
								}
								required
							/>
						</label>
						<button type="submit" disabled={controller.busy}>
							{t("oqtoUi.gitlab.submitReviewForApproval")}
						</button>
					</form>
				)}
			{controller.replyEventId === event.id &&
				event.discussionId &&
				event.entityKind === "merge_request" && (
					<form
						className="wb-gitlab__form"
						onSubmit={(submitEvent) => {
							submitEvent.preventDefault();
							const discussionId = event.discussionId;
							if (event.relatedIssueIid === null || discussionId === null)
								return;
							void controller.propose(event.relatedIssueIid, {
								kind: "discussion_reply",
								sessionId: controller.sessionId,
								mergeRequestIid: event.entityIid,
								discussionId,
								body: controller.replyBody,
							});
						}}
					>
						<label>
							{t("oqtoUi.gitlab.proposedThreadReply")}
							<textarea
								value={controller.replyBody}
								onChange={(changeEvent) =>
									controller.setReplyBody(changeEvent.target.value)
								}
								required
							/>
						</label>
						<button type="submit" disabled={controller.busy}>
							{t("oqtoUi.gitlab.submitReplyForApproval")}
						</button>
					</form>
				)}
		</>
	);
}
