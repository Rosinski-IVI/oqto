import { GitLabInboxPane } from "@/src/oqto-ui/issues/GitLabInboxPane";
import type { GitHost } from "@/src/oqto-ui/platform/git-contract";
import type {
	GitLabHost,
	GitLabInboxEvent,
	GitLabProposal,
	GitLabTicket,
} from "@/src/oqto-ui/platform/gitlab-contract";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const event: GitLabInboxEvent = {
	id: "event-1",
	workspacePath: "/work/repo",
	projectPath: "team/repo",
	category: "assigned_issues",
	categories: ["assigned_issues"],
	entityKind: "issue",
	entityIid: 42,
	relatedIssueIid: null,
	discussionId: null,
	sessionId: null,
	title: "Add the feature",
	summary: "A sample ticket",
	url: "https://gitlab.example/team/repo/-/issues/42",
	occurredAt: "2026-11-04T10:00:00Z",
	readAt: null,
};

const ticket: GitLabTicket = {
	iid: 42,
	id: "42",
	title: "Add the feature",
	status: "open",
	priority: 3,
	kind: "issue",
	parentId: null,
	blockedBy: [],
	labels: [],
	description: "A sample ticket",
	state: "opened",
	webUrl: event.url,
	updatedAt: event.occurredAt,
	statusConflict: false,
	mergeRequestIid: null,
};

describe("GitLab inbox workflow", () => {
	it("plans in the current session and requires an approval before issue writes", async () => {
		let proposals: GitLabProposal[] = [];
		const pending: GitLabProposal = {
			id: "proposal-1",
			workspacePath: "/work/repo",
			projectPath: "team/repo",
			issueIid: 42,
			sessionId: "session-1",
			kind: "comment",
			payload: { body: "A proposed comment" },
			status: "pending",
			createdAt: event.occurredAt,
			sessionStale: false,
		};
		const host: GitLabHost = {
			getConfig: async () => ({
				host: "gitlab.example",
				pathPrefix: "",
				credentialId: "gitlab",
				revision: 1,
				enabled: true,
				eventCategories: ["assigned_issues"],
				credentialReady: true,
				connected: true,
				gitlabUsername: "alice",
			}),
			saveConfig: vi.fn(),
			testConnection: vi.fn(async () => ({
				username: "alice",
				credentialRevision: "1",
			})),
			disconnect: vi.fn(),
			getMapping: async () => ({
				workspacePath: "/work/repo",
				projectPath: "team/repo",
				source: "remote",
			}),
			saveMapping: vi.fn(),
			removeMapping: vi.fn(),
			inbox: async () => [event],
			listIssues: async () => ({ issues: [ticket], truncated: false }),
			selectIssue: vi.fn(),
			markRead: vi.fn(),
			unreadCounts: vi.fn(async () => ({ total: 0, byWorkspacePath: {} })),
			proposals: async () => proposals,
			createProposal: vi.fn(async () => {
				proposals = [pending];
				return pending;
			}),
			approveProposal: vi.fn(async () => {
				const applied = { ...pending, status: "applied" as const };
				proposals = [applied];
				return applied;
			}),
			rebindProposal: vi.fn(async () => pending),
			rejectProposal: vi.fn(async () => {
				proposals = [{ ...pending, status: "rejected" }];
			}),
			createOrUpdateDraft: vi.fn(),
			markMergeRequestReady: vi.fn(),
		};
		const gitHost: GitHost = {
			status: vi.fn(),
			diff: vi.fn(),
			log: vi.fn(),
			stage: vi.fn(),
			commit: vi.fn(),
			branches: { list: vi.fn(), switch: vi.fn() },
			remote: vi.fn(),
			remotes: async () => [
				{ name: "origin", url: "https://gitlab.example/team/repo.git" },
			],
		};
		const onPlan = vi.fn();
		const onStartImplementation = vi.fn();
		const onAddressComment = vi.fn();
		render(
			<GitLabInboxPane
				host={host}
				gitHost={gitHost}
				workspacePath="/work/repo"
				sessionId="session-1"
				onPlan={onPlan}
				onStartImplementation={onStartImplementation}
				onAddressComment={onAddressComment}
			/>,
		);

		await screen.findByText("Add the feature");
		fireEvent.click(screen.getByRole("button", { name: "Activity inbox" }));
		fireEvent.click(screen.getByRole("button", { name: "Plan in session" }));
		expect(onPlan).toHaveBeenCalledWith(event);

		fireEvent.click(screen.getByRole("button", { name: "Propose comment" }));
		fireEvent.change(screen.getByLabelText("Comment"), {
			target: { value: "A proposed comment" },
		});
		fireEvent.click(
			screen.getByRole("button", { name: "Submit for approval" }),
		);
		await screen.findByText("comment for issue #42");
		expect(host.createProposal).toHaveBeenCalledWith({
			workspacePath: "/work/repo",
			issueIid: 42,
			proposal: {
				kind: "comment",
				sessionId: "session-1",
				body: "A proposed comment",
			},
		});
		expect(host.approveProposal).not.toHaveBeenCalled();

		fireEvent.click(screen.getByRole("button", { name: "Approve" }));
		await waitFor(() =>
			expect(host.approveProposal).toHaveBeenCalledWith("proposal-1"),
		);
	});
});
