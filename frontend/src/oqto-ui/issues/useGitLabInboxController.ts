import { useGitLabInboxWatch } from "@/hooks/use-gitlab-inbox-watch";
import { useMountEffect } from "@/hooks/use-mount-effect";
import { type FormEvent, useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import type { GitHost, GitRemote } from "../platform/git-contract";
import type {
	GitLabConnection,
	GitLabEventCategory,
	GitLabHost,
	GitLabInboxEvent,
	GitLabProposal,
	GitLabTicket,
} from "../platform/gitlab-contract";
import { DEFAULT_GITLAB_CATEGORIES } from "./gitlab-inbox-constants";
import {
	preferredRemote,
	preferredRemoteEntry,
	proposedIssueChanges,
	remoteHost,
	remoteProject,
	ticketAsEvent,
} from "./gitlab-inbox-utils";

export interface GitLabInboxControllerArgs {
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

export function useGitLabInboxController({
	host,
	gitHost,
	workspacePath,
	sessionId,
	onPlan,
	onStartImplementation,
	onAddressComment,
}: GitLabInboxControllerArgs) {
	const { t } = useTranslation();
	const [connection, setConnection] = useState<GitLabConnection | null>(null);
	const [events, setEvents] = useState<readonly GitLabInboxEvent[]>([]);
	const [tickets, setTickets] = useState<readonly GitLabTicket[]>([]);
	const [issuesTruncated, setIssuesTruncated] = useState(false);
	const [proposals, setProposals] = useState<readonly GitLabProposal[]>([]);
	const [remotes, setRemotes] = useState<readonly GitRemote[]>([]);
	const [instanceUrl, setInstanceUrl] = useState("");
	const [selectedCategories, setSelectedCategories] = useState<
		readonly GitLabEventCategory[]
	>(DEFAULT_GITLAB_CATEGORIES);
	const [remoteUrl, setRemoteUrl] = useState("");
	const [currentBranch, setCurrentBranch] = useState("");
	const [draftRemoteName, setDraftRemoteName] = useState("");
	const [mappingSource, setMappingSource] = useState<"remote" | "override">(
		"remote",
	);
	const [editingMapping, setEditingMapping] = useState(false);
	const [projectOverride, setProjectOverride] = useState("");
	const [commentIssue, setCommentIssue] = useState<number | null>(null);
	const [comment, setComment] = useState("");
	const [editIssue, setEditIssue] = useState<number | null>(null);
	const [editTitle, setEditTitle] = useState("");
	const [editDescription, setEditDescription] = useState("");
	const [reviewIssue, setReviewIssue] = useState<number | null>(null);
	const [reviewMrIid, setReviewMrIid] = useState("");
	const [reviewPath, setReviewPath] = useState("");
	const [reviewLine, setReviewLine] = useState("");
	const [reviewBody, setReviewBody] = useState("");
	const [reviewSide, setReviewSide] = useState<"old" | "new">("new");
	const [replyEventId, setReplyEventId] = useState<string | null>(null);
	const [replyBody, setReplyBody] = useState("");
	const [targetBranch, setTargetBranch] = useState("");
	const [mrIssue, setMrIssue] = useState<number | null>(null);
	const [gitlabView, setGitlabView] = useState<"issues" | "inbox">("issues");
	const [showClosedIssues, setShowClosedIssues] = useState(false);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	const matchingRemotes = connection
		? remotes.filter(
				(remote) =>
					remoteHost(remote.url) === connection.host &&
					(!projectOverride ||
						remoteProject(remote.url, connection.pathPrefix) ===
							projectOverride),
			)
		: remotes;

	const refreshInbox = useCallback(
		async (poll = false) => {
			let latestConnection = await host.getConfig();
			setConnection(latestConnection);
			if (!latestConnection) return;
			if (!latestConnection.connected) {
				if (!latestConnection.credentialReady) return;
				const result = await host.testConnection(workspacePath);
				latestConnection = {
					...latestConnection,
					connected: true,
					gitlabUsername: result.username,
				};
				setConnection(latestConnection);
			}
			const [nextEvents, nextProposals, issueList] = await Promise.all([
				host.inbox(workspacePath, poll),
				host.proposals(workspacePath),
				host.listIssues(workspacePath),
			]);
			setEvents(nextEvents);
			setProposals(nextProposals);
			setTickets(issueList.issues);
			setIssuesTruncated(issueList.truncated);
		},
		[host, workspacePath],
	);

	useMountEffect(() => {
		let active = true;
		void (async () => {
			try {
				const [config, mapping, nextRemotes, gitStatus] = await Promise.all([
					host.getConfig(),
					host.getMapping(workspacePath),
					gitHost?.remotes?.(workspacePath) ?? Promise.resolve([]),
					gitHost?.status(workspacePath) ?? Promise.resolve(null),
				]);
				if (!active) return;
				setConnection(config);
				setInstanceUrl(
					config ? `https://${config.host}${config.pathPrefix}` : "",
				);
				if (config) setSelectedCategories(config.eventCategories);
				setRemotes(nextRemotes);
				setCurrentBranch(gitStatus?.branch ?? "");
				setRemoteUrl(
					config
						? preferredRemote(nextRemotes, config.host)
						: (nextRemotes.find((remote) => remote.name === "origin")?.url ??
								""),
				);
				if (mapping) {
					setProjectOverride(mapping.projectPath);
					setMappingSource(mapping.source);
					const mappedRemote = preferredRemoteEntry(
						nextRemotes,
						config?.host ?? "",
						config?.pathPrefix ?? "",
						mapping.projectPath,
					);
					setDraftRemoteName(mappedRemote?.name ?? "");
					if (mapping.source === "remote" && mappedRemote)
						setRemoteUrl(mappedRemote.url);
				}
				if (config?.credentialReady && mapping) {
					let connected = config.connected;
					if (!connected) {
						const result = await host.testConnection(workspacePath);
						connected = true;
						if (active) {
							setConnection((current) =>
								current
									? {
											...current,
											connected: true,
											gitlabUsername: result.username,
										}
									: current,
							);
						}
					}
					if (!connected) return;
					await refreshInbox(true);
				}
			} catch (reason) {
				if (active)
					setError(errorMessage(reason, t("oqtoUi.gitlab.requestFailed")));
			}
		})();
		return () => {
			active = false;
		};
	});

	const reportError = useCallback((detail: string) => setError(detail), []);
	const refreshForWatch = useCallback(() => refreshInbox(), [refreshInbox]);
	useGitLabInboxWatch({
		host,
		connected: connection?.connected === true,
		projectPath: projectOverride,
		workspacePath,
		refresh: refreshForWatch,
		onError: reportError,
	});

	async function run(action: () => Promise<void>) {
		if (busy) return;
		setBusy(true);
		setError("");
		try {
			await action();
		} catch (reason) {
			setError(errorMessage(reason, t("oqtoUi.gitlab.requestFailed")));
		} finally {
			setBusy(false);
		}
	}

	async function saveConnection(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		await run(async () => {
			const next = await host.saveConfig({
				instanceUrl,
				enabled: true,
				eventCategories: selectedCategories,
			});
			setConnection(next);
			setRemoteUrl(preferredRemote(remotes, next.host));
			if (next.credentialReady) {
				const result = await host.testConnection(workspacePath);
				setConnection((current) =>
					current
						? {
								...current,
								connected: true,
								gitlabUsername: result.username,
							}
						: current,
				);
				if (projectOverride) await refreshInbox(true);
			}
		});
	}

	async function saveNotificationSettings(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		if (!connection) return;
		await run(async () => {
			const next = await host.saveConfig({
				instanceUrl: `https://${connection.host}${connection.pathPrefix}`,
				enabled: true,
				eventCategories: selectedCategories,
			});
			setConnection(next);
			await refreshInbox(true);
		});
	}

	async function testConnection() {
		await run(async () => {
			const result = await host.testConnection(workspacePath);
			setConnection((current) =>
				current
					? {
							...current,
							connected: true,
							credentialReady: true,
							gitlabUsername: result.username,
						}
					: current,
			);
			if (projectOverride) await refreshInbox(true);
		});
	}

	async function saveMapping(event: FormEvent<HTMLFormElement>) {
		event.preventDefault();
		await run(async () => {
			const mapping = projectOverride.trim()
				? await host.saveMapping({
						workspacePath,
						projectPathOverride: projectOverride.trim(),
					})
				: await host.saveMapping({ workspacePath, remoteUrl });
			setProjectOverride(mapping.projectPath);
			setMappingSource(mapping.source);
			setEditingMapping(false);
			if (connection?.connected) await refreshInbox(true);
		});
	}

	async function removeMapping() {
		if (!window.confirm(t("oqtoUi.gitlab.confirmRemoveMapping"))) return;
		await run(async () => {
			await host.removeMapping(workspacePath);
			setProjectOverride("");
			setMappingSource("remote");
			setEditingMapping(false);
			setEvents([]);
			setTickets([]);
			setProposals([]);
		});
	}

	async function disconnect() {
		await run(async () => {
			await host.disconnect();
			setConnection(null);
			setEvents([]);
			setTickets([]);
			setProposals([]);
			setProjectOverride("");
			setMappingSource("remote");
			setEditingMapping(false);
		});
	}

	async function propose(
		issueIid: number,
		proposal: Parameters<GitLabHost["createProposal"]>[0]["proposal"],
	) {
		await run(async () => {
			await host.createProposal({ workspacePath, issueIid, proposal });
			setCommentIssue(null);
			setComment("");
			setEditIssue(null);
			setReviewIssue(null);
			setReplyEventId(null);
			setReplyBody("");
			await refreshInbox();
		});
	}

	async function decideProposal(proposal: GitLabProposal, approve: boolean) {
		await run(async () => {
			let result: GitLabProposal | null = null;
			if (approve) {
				try {
					result = await host.approveProposal(proposal.id);
				} catch (reason) {
					const detail = errorMessage(reason, t("oqtoUi.gitlab.requestFailed"));
					if (!detail.includes("GitLab ticket changed since this proposal.")) {
						await refreshInbox();
						throw reason;
					}
					const changes = proposedIssueChanges(proposal);
					const summary = changes?.length
						? t("oqtoUi.gitlab.proposedChanges", {
								changes: changes.join(" · "),
							})
						: t("oqtoUi.gitlab.genericProposal");
					if (
						!window.confirm(
							t("oqtoUi.gitlab.confirmOverwrite", { summary, detail }),
						)
					)
						return;
					result = await host.approveProposal(proposal.id, true);
				}
			} else {
				await host.rejectProposal(proposal.id);
			}
			await refreshInbox();
			if (approve && result?.kind === "start_implementation") {
				const issueEvent = events.find(
					(item) =>
						item.entityKind === "issue" && item.entityIid === result.issueIid,
				);
				const ticket = tickets.find((item) => item.iid === result.issueIid);
				const selectedEvent =
					issueEvent ??
					(ticket
						? ticketAsEvent(ticket, projectOverride, workspacePath)
						: null);
				if (selectedEvent)
					onStartImplementation(selectedEvent, result.sessionId);
			}
		});
	}

	async function rebindProposal(proposal: GitLabProposal) {
		await run(async () => {
			await host.rebindProposal(proposal.id, sessionId);
			await refreshInbox();
		});
	}

	async function markEventRead(item: GitLabInboxEvent) {
		try {
			await host.markRead(item.id);
			setEvents((current) =>
				current.map((event) =>
					event.id === item.id
						? { ...event, readAt: new Date().toISOString() }
						: event,
				),
			);
		} catch {
			// Keep the event actionable if acknowledging read fails.
		}
	}

	async function routeEventToSession(
		item: GitLabInboxEvent,
		action: () => void,
	) {
		await markEventRead(item);
		action();
	}

	async function proposeTicketStatus(
		ticket: GitLabTicket,
		status: GitLabTicket["status"],
	) {
		const workflowLabels = [
			"oqto::ready",
			"oqto::in_progress",
			"oqto::blocked",
		];
		const labels = ticket.labels.filter(
			(label) => !workflowLabels.includes(label),
		);
		if (status === "ready") labels.push("oqto::ready");
		if (status === "in_progress") labels.push("oqto::in_progress");
		if (status === "blocked") labels.push("oqto::blocked");
		const stateEvent =
			status === "closed"
				? "close"
				: ticket.state === "closed"
					? "reopen"
					: undefined;
		await propose(ticket.iid, {
			kind: "update_issue",
			sessionId,
			labels,
			...(stateEvent ? { stateEvent } : {}),
		});
	}

	async function makeDraft(event: GitLabInboxEvent) {
		const issueIid = event.relatedIssueIid ?? event.entityIid;
		const remote = matchingRemotes.find(
			(candidate) => candidate.name === draftRemoteName,
		);
		if (!remote) {
			setError(t("oqtoUi.gitlab.chooseMatchingRemote"));
			return;
		}
		if (!currentBranch) {
			setError(t("oqtoUi.gitlab.featureBranchRequired"));
			return;
		}
		if (
			!window.confirm(t("oqtoUi.gitlab.confirmDraft", { title: event.title }))
		)
			return;
		await run(async () => {
			const title = window.prompt(t("oqtoUi.gitlab.draftTitle"), event.title);
			if (!title) return;
			await host.createOrUpdateDraft({
				workspacePath,
				issueIid,
				remoteName: remote.name,
				...(targetBranch.trim() ? { targetBranch: targetBranch.trim() } : {}),
				title,
				description: `Implementation for ${event.title}`,
				confirmRedraftReady: true,
			});
			setMrIssue(null);
			setError(t("oqtoUi.gitlab.draftSaved"));
			await refreshInbox();
		});
	}

	async function ready(event: GitLabInboxEvent) {
		const issueIid = event.relatedIssueIid ?? event.entityIid;
		if (!window.confirm(t("oqtoUi.gitlab.confirmReady"))) return;
		await run(async () => {
			await host.markMergeRequestReady({
				workspacePath,
				issueIid,
				confirmed: true,
			});
			setError(t("oqtoUi.gitlab.mergeRequestReady"));
			await refreshInbox();
		});
	}

	const appliedStarts = new Set(
		proposals
			.filter(
				(proposal) =>
					proposal.kind === "start_implementation" &&
					proposal.status === "applied",
			)
			.map((proposal) => proposal.issueIid),
	);

	return {
		sessionId,
		workspacePath,
		connection,
		events,
		tickets,
		issuesTruncated,
		proposals,
		remotes,
		instanceUrl,
		setInstanceUrl,
		selectedCategories,
		setSelectedCategories,
		remoteUrl,
		setRemoteUrl,
		currentBranch,
		draftRemoteName,
		setDraftRemoteName,
		mappingSource,
		editingMapping,
		setEditingMapping,
		projectOverride,
		setProjectOverride,
		commentIssue,
		setCommentIssue,
		comment,
		setComment,
		editIssue,
		setEditIssue,
		editTitle,
		setEditTitle,
		editDescription,
		setEditDescription,
		reviewIssue,
		setReviewIssue,
		reviewMrIid,
		setReviewMrIid,
		reviewPath,
		setReviewPath,
		reviewLine,
		setReviewLine,
		reviewBody,
		setReviewBody,
		reviewSide,
		setReviewSide,
		replyEventId,
		setReplyEventId,
		replyBody,
		setReplyBody,
		targetBranch,
		setTargetBranch,
		mrIssue,
		setMrIssue,
		gitlabView,
		setGitlabView,
		showClosedIssues,
		setShowClosedIssues,
		busy,
		error,
		matchingRemotes,
		appliedStarts,
		refreshInbox,
		run,
		saveConnection,
		saveNotificationSettings,
		testConnection,
		saveMapping,
		removeMapping,
		disconnect,
		propose,
		decideProposal,
		rebindProposal,
		markEventRead,
		routeEventToSession,
		proposeTicketStatus,
		makeDraft,
		ready,
		onPlan,
		onAddressComment,
	};
}

export type GitLabInboxController = ReturnType<typeof useGitLabInboxController>;

function errorMessage(reason: unknown, fallback: string): string {
	if (reason instanceof Error) return reason.message || fallback;
	return typeof reason === "string" && reason ? reason : fallback;
}
