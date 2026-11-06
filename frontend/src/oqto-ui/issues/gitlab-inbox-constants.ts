import type {
	GitLabEventCategory,
	GitLabTicket,
} from "../platform/gitlab-contract";

export const GITLAB_CATEGORIES: readonly {
	id: GitLabEventCategory;
	labelKey: string;
}[] = [
	{ id: "assigned_issues", labelKey: "assignedIssues" },
	{ id: "authored_issues", labelKey: "authoredIssues" },
	{ id: "mentions_review_requests", labelKey: "mentionsReviewRequests" },
	{ id: "project_activity", labelKey: "projectActivity" },
];

export const DEFAULT_GITLAB_CATEGORIES = GITLAB_CATEGORIES.filter(
	({ id }) => id !== "project_activity",
).map(({ id }) => id);

export const GITLAB_TICKET_STATUSES: readonly GitLabTicket["status"][] = [
	"in_progress",
	"blocked",
	"ready",
	"open",
	"closed",
];
