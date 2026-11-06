import { type FormEvent, useCallback, useEffect, useState } from "react";
import type { GitHost, GitRemote } from "../platform/git-contract";
import type {
	GitLabConnection,
	GitLabEventCategory,
	GitLabHost,
	GitLabInboxEvent,
	GitLabProposal,
	GitLabTicket,
} from "../platform/gitlab-contract";

const CATEGORIES: readonly {
	id: GitLabEventCategory;
	label: string;
}[] = [
	{ id: "assigned_issues", label: "Assigned issues" },
	{ id: "authored_issues", label: "Authored issues" },
	{ id: "mentions_review_requests", label: "Mentions and review requests" },
	{ id: "project_activity", label: "Project activity" },
];
const DEFAULT_CATEGORIES = CATEGORIES.filter(({ id }) => id !== "project_activity").map(({ id }) => id);
const TICKET_STATUSES = ["in_progress", "blocked", "ready", "open", "closed"] as const;

interface GitLabInboxPaneProps {
	readonly host: GitLabHost;
	readonly gitHost?: GitHost;
	readonly workspacePath: string;
	readonly sessionId: string;
	readonly onPlan: (event: GitLabInboxEvent) => void;
	readonly onStartImplementation: (event: GitLabInboxEvent, sessionId: string | null) => void;
	readonly onAddressComment: (event: GitLabInboxEvent) => void;
}

export function GitLabInboxPane({
	host,
	gitHost,
	workspacePath,
	sessionId,
	onPlan,
	onStartImplementation,
	onAddressComment,
}: GitLabInboxPaneProps) {
	const [connection, setConnection] = useState<GitLabConnection | null>(null);
	const [events, setEvents] = useState<readonly GitLabInboxEvent[]>([]);
	const [tickets, setTickets] = useState<readonly GitLabTicket[]>([]);
	const [issuesTruncated, setIssuesTruncated] = useState(false);
	const [proposals, setProposals] = useState<readonly GitLabProposal[]>([]);
	const [remotes, setRemotes] = useState<readonly GitRemote[]>([]);
	const [instanceUrl, setInstanceUrl] = useState("");
	const [selectedCategories, setSelectedCategories] = useState<readonly GitLabEventCategory[]>(
		DEFAULT_CATEGORIES,
	);
	const [remoteUrl, setRemoteUrl] = useState("");
	const [currentBranch, setCurrentBranch] = useState("");
	const [draftRemoteName, setDraftRemoteName] = useState("");
	const [mappingSource, setMappingSource] = useState<"remote" | "override">("remote");
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
		? remotes.filter((remote) =>
			remoteHost(remote.url) === connection.host &&
			(!projectOverride || remoteProject(remote.url, connection.pathPrefix) === projectOverride),
		)
		: remotes;

	const refreshInbox = useCallback(async (poll = false) => {
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
	}, [host, workspacePath]);

	useEffect(() => {
		let active = true;
		void (async () => {
			try {
				const [config, mapping, remotes, gitStatus] = await Promise.all([
					host.getConfig(),
					host.getMapping(workspacePath),
					gitHost?.remotes?.(workspacePath) ?? Promise.resolve([]),
					gitHost?.status(workspacePath) ?? Promise.resolve(null),
				]);
				if (!active) return;
				setConnection(config);
				setInstanceUrl(config ? `https://${config.host}${config.pathPrefix}` : "");
				if (config) setSelectedCategories(config.eventCategories);
				setRemotes(remotes);
				setCurrentBranch(gitStatus?.branch ?? "");
				setRemoteUrl(
					config
						? preferredRemote(remotes, config.host)
						: (remotes.find((remote) => remote.name === "origin")?.url ?? ""),
				);
				if (mapping) {
					setProjectOverride(mapping.projectPath);
					setMappingSource(mapping.source);
					const mappedRemote = preferredRemoteEntry(
						remotes,
						config?.host ?? "",
						config?.pathPrefix ?? "",
						mapping.projectPath,
					);
					setDraftRemoteName(mappedRemote?.name ?? "");
					if (mapping.source === "remote" && mappedRemote) setRemoteUrl(mappedRemote.url);
				}
				if (config?.credentialReady && mapping) {
					let connected = config.connected;
					if (!connected) {
						const result = await host.testConnection(workspacePath);
						connected = true;
						if (active) {
							setConnection((current) => current ? {
								...current,
								connected: true,
								gitlabUsername: result.username,
							} : current);
						}
					}
					if (!connected) return;
					await refreshInbox(true);
				}
			} catch (reason) {
				if (active) setError(message(reason));
			}
		})();
		return () => {
			active = false;
		};
	}, [host, gitHost, workspacePath, refreshInbox]);

	useEffect(() => {
		if (!connection || !projectOverride || !host.watchInboxUpdates) return;
		return host.watchInboxUpdates((counts) => {
			if (!(workspacePath in counts.byWorkspacePath)) return;
			void refreshInbox().catch((reason: unknown) => setError(message(reason)));
		});
	}, [connection?.connected, host, projectOverride, refreshInbox, workspacePath]);

	async function run(action: () => Promise<void>) {
		if (busy) return;
		setBusy(true);
		setError("");
		try {
			await action();
		} catch (reason) {
			setError(message(reason));
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
				setConnection((current) => current ? {
					...current,
					connected: true,
					gitlabUsername: result.username,
				} : current);
				if (projectOverride) {
					await refreshInbox(true);
				}
			}
		});
	}

	async function testConnection() {
		await run(async () => {
			const result = await host.testConnection(workspacePath);
			setConnection((current) => current ? {
				...current,
				connected: true,
				credentialReady: true,
				gitlabUsername: result.username,
			} : current);
			if (projectOverride) {
				await refreshInbox(true);
			}
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
			if (connection?.connected) {
				await refreshInbox(true);
			}
		});
	}

	async function removeMapping() {
		if (!window.confirm("Remove the GitLab project mapping and its cached inbox/workflow links for this workspace?")) return;
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
					const detail = message(reason);
					if (!detail.includes("GitLab ticket changed since this proposal.")) {
						await refreshInbox();
						throw reason;
					}
					if (!window.confirm(`${proposalSummary(proposal)}\n\n${detail}\n\nOverwrite the conflicting field(s)?`)) return;
					result = await host.approveProposal(proposal.id, true);
				}
			} else {
				await host.rejectProposal(proposal.id);
			}
			await refreshInbox();
			if (approve && result?.kind === "start_implementation") {
				const issueEvent = events.find(
					(item) => item.entityKind === "issue" && item.entityIid === result.issueIid,
				);
				const ticket = tickets.find((item) => item.iid === result.issueIid);
				const event = issueEvent ?? (ticket ? ticketAsEvent(ticket, projectOverride, workspacePath) : null);
				if (event) onStartImplementation(event, result.sessionId);
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
			setEvents((current) => current.map((event) =>
				event.id === item.id ? { ...event, readAt: new Date().toISOString() } : event,
			));
		} catch {
			// Keep the event actionable if acknowledging read fails.
		}
	}

	async function routeEventToSession(item: GitLabInboxEvent, action: () => void) {
		await markEventRead(item);
		action();
	}

	async function proposeTicketStatus(ticket: GitLabTicket, status: GitLabTicket["status"]) {
		const workflowLabels = ["oqto::ready", "oqto::in_progress", "oqto::blocked"];
		const labels = ticket.labels.filter((label) => !workflowLabels.includes(label));
		if (status === "ready") labels.push("oqto::ready");
		if (status === "in_progress") labels.push("oqto::in_progress");
		if (status === "blocked") labels.push("oqto::blocked");
		const stateEvent = status === "closed"
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
		const remote = matchingRemotes.find((candidate) => candidate.name === draftRemoteName);
		if (!remote) {
			setError("Choose a Git remote that matches this GitLab project before pushing.");
			return;
		}
		if (!currentBranch) {
			setError("A current feature branch is required to create a draft merge request.");
			return;
		}
		if (!window.confirm("Push the current committed feature branch with normal Git authentication and create/update a draft MR? Updating a ready MR will move it back to draft.")) return;
		await run(async () => {
			const title = window.prompt("Draft MR title", event.title);
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
			setError("Draft merge request saved.");
			await refreshInbox();
		});
	}

	async function ready(event: GitLabInboxEvent) {
		const issueIid = event.relatedIssueIid ?? event.entityIid;
		if (!window.confirm("Mark this draft merge request ready for review?")) return;
		await run(async () => {
			await host.markMergeRequestReady({
				workspacePath,
				issueIid,
				confirmed: true,
			});
			setError("Merge request marked ready.");
			await refreshInbox();
		});
	}

	if (!connection) {
		return (
			<section className="wb-issues__body" aria-label="GitLab setup">
				<h3>Connect GitLab</h3>
				<p>
					Save a personal access token in <strong>Host credentials</strong> with
					credential ID <code>gitlab</code>. The token stays in the host vault and
					is never sent to the browser or agent.
				</p>
				<p>GitLab v1 is available only in personal workspaces, not shared or remote-machine workspaces.</p>
				<form onSubmit={saveConnection} className="wb-gitlab__form">
					<label>
						GitLab HTTPS origin
						<input
							type="url"
							value={instanceUrl}
							placeholder="https://gitlab.example.com"
							onChange={(event) => setInstanceUrl(event.target.value)}
							required
						/>
					</label>
					<fieldset>
						<legend>Notification categories</legend>
						{CATEGORIES.map(({ id, label }) => (
							<label key={id}>
								<input
									type="checkbox"
									checked={selectedCategories.includes(id)}
									onChange={(event) => setSelectedCategories((current) => event.target.checked ? [...current, id] : current.filter((value) => value !== id))}
								/>
								{label}
							</label>
						))}
					</fieldset>
					<button type="submit" disabled={busy}>Connect instance</button>
				</form>
				{error && <p role="alert">{error}</p>}
			</section>
		);
	}

	const connected = connection;
	const appliedStarts = new Set(
		proposals
			.filter((proposal) => proposal.kind === "start_implementation" && proposal.status === "applied")
			.map((proposal) => proposal.issueIid),
	);

	return (
		<div className="wb-gitlab">
			<div className="wb-gitlab__bar">
				<strong>https://{connected.host}{connected.pathPrefix}{connected.gitlabUsername ? ` · @${connected.gitlabUsername}` : ""}</strong>
				{!connected.connected ? (
					<button type="button" disabled={busy} onClick={() => void testConnection()}>
						Test connection
					</button>
				) : (
					<button type="button" disabled={busy || !projectOverride} onClick={() => void run(() => refreshInbox(true))}>
					Refresh inbox
					</button>
				)}
				<button
					type="button"
					disabled={busy}
					onClick={() => void run(async () => {
						await host.disconnect();
						setConnection(null);
						setEvents([]);
						setTickets([]);
						setProposals([]);
						setProjectOverride("");
						setMappingSource("remote");
						setEditingMapping(false);
					})}
				>
					Disconnect
				</button>
			</div>
			<details className="wb-gitlab__settings">
				<summary>Notification settings</summary>
				<form onSubmit={(event) => {
					event.preventDefault();
					void run(async () => {
					const next = await host.saveConfig({ instanceUrl: `https://${connected.host}${connected.pathPrefix}`, enabled: true, eventCategories: selectedCategories });
						setConnection(next);
						await refreshInbox(true);
					});
				}} className="wb-gitlab__form">
					{CATEGORIES.map(({ id, label }) => (
						<label key={id}>
							<input
								type="checkbox"
								checked={selectedCategories.includes(id)}
								onChange={(event) => setSelectedCategories((current) => event.target.checked ? [...current, id] : current.filter((value) => value !== id))}
							/>
							{label}
						</label>
					))}
					<button type="submit" disabled={busy}>Save notifications</button>
				</form>
			</details>
			{!connected.credentialReady && (
				<p role="alert">
					The <code>gitlab</code> host credential is missing or the vault is locked.
					Save/unlock it in Host credentials; no token value is needed here.
				</p>
			)}
			{connected.credentialReady && !connected.connected && (
				<p role="alert">Test the GitLab connection before polling or writing.</p>
			)}
			<p className="wb-gitlab__scope">Personal workspaces only; shared and remote-machine workspaces are not supported.</p>
			{(!projectOverride || editingMapping) && (
				<form onSubmit={saveMapping} className="wb-gitlab__form">
					<h3>Map this workspace</h3>
					<label>
						Git remote (automatic mapping)
						{matchingRemotes.length > 0 ? (
							<select value={remoteUrl} onChange={(event) => setRemoteUrl(event.target.value)}>
								<option value="">Select a matching remote…</option>
								{matchingRemotes.map((remote) => (
									<option key={remote.name} value={remote.url}>
										{remote.name}: {remote.url}
									</option>
								))}
							</select>
						) : (
							<input value={remoteUrl} onChange={(event) => setRemoteUrl(event.target.value)} placeholder="https://gitlab.example.com/group/project.git" />
						)}
					</label>
					<p>Or set an explicit project override:</p>
					<label>
						Project path
						<input value={projectOverride} onChange={(event) => setProjectOverride(event.target.value)} placeholder="group/subgroup/project" />
					</label>
					<button type="submit" disabled={busy || (!remoteUrl && !projectOverride)}>Save project mapping</button>
				</form>
			)}
			{projectOverride && !editingMapping && (
				<div className="wb-gitlab__bar">
					<span>Project: <code>{projectOverride}</code> · {mappingSource === "override" ? "explicit override" : "Git remote"}</span>
					<button type="button" onClick={() => { if (mappingSource === "remote") setProjectOverride(""); setEditingMapping(true); }}>Change mapping</button>
					<button type="button" disabled={busy} onClick={() => void removeMapping()}>Remove mapping</button>
				</div>
			)}
			{projectOverride && !editingMapping && (
				<nav className="wb-gitlab__views" aria-label="GitLab views">
					<button type="button" aria-pressed={gitlabView === "issues"} onClick={() => setGitlabView("issues")}>Issues</button>
					<button type="button" aria-pressed={gitlabView === "inbox"} onClick={() => setGitlabView("inbox")}>Activity inbox</button>
					<button type="button" aria-pressed={showClosedIssues} onClick={() => setShowClosedIssues((value) => !value)}>
						{showClosedIssues ? "Hide closed" : "Show closed"}
					</button>
				</nav>
			)}
			{error && <p role="alert" className="wb-gitlab__error">{error}</p>}
			{proposals.some((proposal) => proposal.status === "pending" || (proposal.status === "approved" && proposal.kind === "inline_review")) && (
                <section className="wb-gitlab__proposals" aria-label="GitLab approval proposals">
					<h3>Approval required</h3>
					{proposals.filter((proposal) => proposal.status === "pending" || (proposal.status === "approved" && proposal.kind === "inline_review")).map((proposal) => (
						<div className="wb-gitlab__proposal" key={proposal.id}>
							<div>
								<strong>{proposal.kind.replaceAll("_", " ")} for issue #{proposal.issueIid}</strong>
								<p>{proposalSummary(proposal)}</p>
								{proposal.sessionStale && <p role="alert">The bound session is unavailable or no longer belongs to this workspace. Rebind and approve again.</p>}
							</div>
							{proposal.sessionStale && <button type="button" disabled={busy || !sessionId} onClick={() => void rebindProposal(proposal)}>Rebind to this session</button>}
							{!proposal.sessionStale && <button type="button" disabled={busy} onClick={() => void decideProposal(proposal, true)}>{proposal.status === "approved" ? "Resume remaining review comments" : "Approve"}</button>}
							{proposal.status === "pending" && <button type="button" disabled={busy} onClick={() => void decideProposal(proposal, false)}>Reject</button>}
						</div>
					))}
				</section>
				)}
			{projectOverride && !editingMapping && gitlabView === "issues" && (
				<section className="wb-gitlab__tickets" aria-label="GitLab issues">
					{issuesTruncated && <p role="status">Showing the first issue pages; narrow your assigned/authored set to see older tickets.</p>}
					{tickets.filter((ticket) => showClosedIssues || ticket.status !== "closed").length === 0
						? <p>No assigned or authored GitLab issues.</p>
						: TICKET_STATUSES.map((status) => {
							const group = tickets.filter((ticket) => ticket.status === status && (showClosedIssues || status !== "closed"));
							if (group.length === 0) return null;
							return (
								<section key={status} className="wb-issues__group">
									<h3>{status.replaceAll("_", " ")}</h3>
									<ul>
										{group.map((ticket) => (
											<li key={ticket.iid} data-status={ticket.status}>
												<span className="wb-issues__id">#{ticket.iid}</span>
												<span className="wb-issues__title">{ticket.title}</span>
						{ticket.statusConflict && <small className="wb-gitlab__error">Conflicting Oqto workflow labels</small>}
												<div className="wb-gitlab__actions">
													<button type="button" onClick={() => void routeEventToSession(ticketAsEvent(ticket, projectOverride, workspacePath), () => onPlan(ticketAsEvent(ticket, projectOverride, workspacePath)))}>Plan in session</button>
													<button type="button" disabled={busy || ticket.state === "closed" || ticket.statusConflict} onClick={() => void propose(ticket.iid, { kind: "start_implementation", sessionId })}>Propose start</button>
													<button type="button" onClick={() => { setCommentIssue(ticket.iid); setEditIssue(null); }}>Propose comment</button>
													<button type="button" onClick={() => { setEditIssue(ticket.iid); setCommentIssue(null); setEditTitle(ticket.title); setEditDescription(ticket.description); }}>Propose edit</button>
													<select aria-label={`Propose status for issue ${ticket.iid}`} value={ticket.status} onChange={(event) => void proposeTicketStatus(ticket, event.target.value as GitLabTicket["status"])} disabled={busy || ticket.statusConflict}>
														{TICKET_STATUSES.map((value) => <option key={value} value={value}>{value.replaceAll("_", " ")}</option>)}
													</select>
													{ticket.mergeRequestIid !== null && appliedStarts.has(ticket.iid) && <>
														<button type="button" onClick={() => setMrIssue(ticket.iid)}>Draft/update MR</button>
														<button type="button" onClick={() => void ready(ticketAsEvent(ticket, projectOverride, workspacePath))}>Mark MR ready…</button>
														<button type="button" onClick={() => { setReviewIssue(ticket.iid); setReviewMrIid(String(ticket.mergeRequestIid)); }}>Propose inline review</button>
													</>}
												</div>
												{ticket.labels.length > 0 && <small>{ticket.labels.join(", ")}</small>}
												{commentIssue === ticket.iid && <form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void propose(ticket.iid, { kind: "comment", sessionId, body: comment }); }}>
													<label>Comment<textarea value={comment} onChange={(event) => setComment(event.target.value)} required /></label>
													<button type="submit" disabled={busy}>Submit for approval</button>
												</form>}
												{editIssue === ticket.iid && <form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void propose(ticket.iid, { kind: "update_issue", sessionId, title: editTitle, ...(editDescription === ticket.description ? {} : { description: editDescription }) }); }}>
													<label>Proposed title<input value={editTitle} onChange={(event) => setEditTitle(event.target.value)} required /></label>
													<label>Proposed description<textarea value={editDescription} onChange={(event) => setEditDescription(event.target.value)} /></label>
													<button type="submit" disabled={busy}>Submit edit for approval</button>
												</form>}
												{mrIssue === ticket.iid && <form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void makeDraft(ticketAsEvent(ticket, projectOverride, workspacePath)); }}>
													<p>Current branch: <code>{currentBranch || "unavailable"}</code></p>
													<label>Git remote<select value={draftRemoteName} onChange={(event) => setDraftRemoteName(event.target.value)} required>
														<option value="">Choose matching remote…</option>
														{matchingRemotes.map((remote) => <option key={remote.name} value={remote.name}>{remote.name}: {remote.url}</option>)}
													</select></label>
													<label>Target branch override<input value={targetBranch} onChange={(event) => setTargetBranch(event.target.value)} placeholder="New MR: project default; updates: keep current" /></label>
													<button type="submit" disabled={busy}>Create/update draft</button>
												</form>}
												{reviewIssue === ticket.iid && ticket.mergeRequestIid !== null && <form className="wb-gitlab__form" onSubmit={(event) => {
													event.preventDefault();
													void propose(ticket.iid, { kind: "inline_review", sessionId, mergeRequestIid: ticket.mergeRequestIid, comments: [{ path: reviewPath, line: Number(reviewLine), side: reviewSide, body: reviewBody }] });
												}}>
													<label>Changed file path<input value={reviewPath} onChange={(event) => setReviewPath(event.target.value)} required /></label>
													<label>Diff side<select value={reviewSide} onChange={(event) => setReviewSide(event.target.value as "old" | "new")}><option value="new">New</option><option value="old">Old</option></select></label>
													<label>Diff line number<input type="number" min="1" value={reviewLine} onChange={(event) => setReviewLine(event.target.value)} required /></label>
													<label>Review comment<textarea value={reviewBody} onChange={(event) => setReviewBody(event.target.value)} required /></label>
													<button type="submit" disabled={busy}>Submit review for approval</button>
												</form>}
										</li>
										))}
									</ul>
								</section>
							);
						})}
				</section>
			)}
			{projectOverride && !editingMapping && gitlabView === "inbox" && <div className="wb-gitlab__events">
				{events.length === 0 ? <p>No GitLab events in this workspace.</p> : events.map((item) => (
					<article className="wb-gitlab__event" key={item.id}>
						<div className="wb-gitlab__event-head">
							<strong>{item.entityKind === "issue" ? "Issue" : "Merge request"} #{item.entityIid}: {item.title}</strong>
							<small>{(item.categories.length > 0 ? item.categories : [item.category]).map((category) => category.replaceAll("_", " ")).join(" · ")}</small>
						</div>
						{item.summary && <p>{item.summary}</p>}
						<div className="wb-gitlab__actions">
								{item.url && <a href={item.url} target="_blank" rel="noreferrer" onClick={() => void host.markRead(item.id)}>Open in GitLab</a>}
								{item.entityKind === "merge_request" && item.relatedIssueIid !== null && (
									<button type="button" onClick={() => void routeEventToSession(item, () => onAddressComment(item))}>Address comment in session</button>
								)}
				{item.entityKind === "merge_request" && item.relatedIssueIid !== null && item.discussionId && (
					<button type="button" onClick={() => void markEventRead(item).then(() => { setReplyEventId(item.id); setReplyBody(""); })}>Propose thread reply</button>
								)}
							{item.entityKind === "issue" && <>
								<button type="button" onClick={() => void routeEventToSession(item, () => onPlan(item))}>Plan in session</button>
								<button type="button" disabled={busy} onClick={() => void propose(item.entityIid, { kind: "start_implementation", sessionId })}>Propose implementation start</button>
								<button type="button" onClick={() => { setCommentIssue(item.entityIid); setEditIssue(null); }}>Propose comment</button>
								<button type="button" onClick={() => { setEditIssue(item.entityIid); setCommentIssue(null); setEditTitle(item.title); }}>Propose issue edit</button>
							</>}
							{(item.entityKind === "issue" && appliedStarts.has(item.entityIid) || item.entityKind === "merge_request" && item.relatedIssueIid !== null && appliedStarts.has(item.relatedIssueIid)) && <>
								<button type="button" onClick={() => setMrIssue(item.relatedIssueIid ?? item.entityIid)}>{item.entityKind === "issue" ? "Draft MR" : "Update draft MR"}</button>
								{item.entityKind === "issue" && <button type="button" onClick={() => setReviewIssue(item.entityIid)}>Propose inline review</button>}
							</>}
						</div>
						{commentIssue === item.entityIid && (
							<form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void propose(item.entityIid, { kind: "comment", sessionId, body: comment }); }}>
								<label>Comment<textarea value={comment} onChange={(event) => setComment(event.target.value)} required /></label>
								<button type="submit" disabled={busy}>Submit for approval</button>
							</form>
						)}
						{editIssue === item.entityIid && (
							<form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void propose(item.entityIid, { kind: "update_issue", sessionId, title: editTitle, ...(editDescription.trim() ? { description: editDescription } : {}) }); }}>
								<label>Proposed title<input value={editTitle} onChange={(event) => setEditTitle(event.target.value)} required /></label>
								<label>Proposed description<textarea value={editDescription} onChange={(event) => setEditDescription(event.target.value)} /></label>
								<button type="submit" disabled={busy}>Submit edit for approval</button>
							</form>
						)}
						{mrIssue === (item.relatedIssueIid ?? item.entityIid) && (
							<form className="wb-gitlab__form" onSubmit={(event) => { event.preventDefault(); void makeDraft(item); }}>
								<p>Current branch: <code>{currentBranch || "unavailable"}</code></p>
								<label>Git remote<select value={draftRemoteName} onChange={(event) => setDraftRemoteName(event.target.value)} required>
									<option value="">Choose matching remote…</option>
									{matchingRemotes.map((remote) => <option key={remote.name} value={remote.name}>{remote.name}: {remote.url}</option>)}
								</select></label>
								<label>Target branch override (optional)<input value={targetBranch} onChange={(event) => setTargetBranch(event.target.value)} placeholder="New MR: project default; updates: keep current" /></label>
								<button type="submit" disabled={busy}>Create/update draft</button>
								<button type="button" disabled={busy} onClick={() => void ready(item)}>Mark draft ready…</button>
							</form>
						)}
						{reviewIssue === item.entityIid && (
							<form className="wb-gitlab__form" onSubmit={(event) => {
								event.preventDefault();
								void propose(item.entityIid, { kind: "inline_review", sessionId, mergeRequestIid: Number(reviewMrIid), comments: [{ path: reviewPath, line: Number(reviewLine), side: reviewSide, body: reviewBody }] });
							}}>
								<label>Merge request IID<input type="number" min="1" value={reviewMrIid} onChange={(event) => setReviewMrIid(event.target.value)} required /></label>
								<label>Changed file path<input value={reviewPath} onChange={(event) => setReviewPath(event.target.value)} required /></label>
								<label>Diff side<select value={reviewSide} onChange={(event) => setReviewSide(event.target.value as "old" | "new")}><option value="new">New</option><option value="old">Old</option></select></label>
								<label>Diff line number<input type="number" min="1" value={reviewLine} onChange={(event) => setReviewLine(event.target.value)} required /></label>
								<label>Review comment<textarea value={reviewBody} onChange={(event) => setReviewBody(event.target.value)} required /></label>
								<button type="submit" disabled={busy}>Submit review for approval</button>
							</form>
						)}
						{replyEventId === item.id && item.discussionId && item.entityKind === "merge_request" && (
							<form className="wb-gitlab__form" onSubmit={(event) => {
								event.preventDefault();
								if (item.relatedIssueIid === null) return;
								void propose(item.relatedIssueIid, {
									kind: "discussion_reply",
									sessionId,
									mergeRequestIid: item.entityIid,
									discussionId: item.discussionId,
									body: replyBody,
								});
							}}>
								<label>Proposed thread reply<textarea value={replyBody} onChange={(event) => setReplyBody(event.target.value)} required /></label>
								<button type="submit" disabled={busy}>Submit reply for approval</button>
							</form>
						)}
					</article>
				))}
			</div>}
		</div>
	);
}

function message(reason: unknown): string {
	return reason instanceof Error ? reason.message : "GitLab request failed.";
}

function remoteHost(remoteUrl: string): string | null {
	if (remoteUrl.startsWith("git@")) {
		const rest = remoteUrl.slice("git@".length);
		const host = rest.split(":", 1)[0];
		return host?.toLowerCase() || null;
	}
	try {
		return new URL(remoteUrl).hostname.toLowerCase();
	} catch {
		return null;
	}
}

function preferredRemote(remotes: readonly GitRemote[], host: string): string {
	const matching = remotes.filter((remote) => remoteHost(remote.url) === host);
	return matching.find((remote) => remote.name === "origin")?.url ?? "";
}

function preferredRemoteEntry(
	remotes: readonly GitRemote[],
	host: string,
	pathPrefix: string,
	projectPath?: string,
): GitRemote | undefined {
	const matching = remotes.filter((remote) =>
		remoteHost(remote.url) === host &&
		(!projectPath || remoteProject(remote.url, pathPrefix) === projectPath),
	);
	return matching.find((remote) => remote.name === "origin");
}

function remoteProject(remoteUrl: string, pathPrefix: string): string | null {
	let rawPath: string;
	let isScp = false;
	if (remoteUrl.startsWith("git@")) {
		rawPath = remoteUrl.slice("git@".length).split(":").slice(1).join(":");
		isScp = true;
	} else {
		try {
			const url = new URL(remoteUrl);
			rawPath = url.pathname.replace(/^\//, "");
			isScp = url.protocol === "ssh:";
		} catch {
			return null;
		}
	}
	const prefix = pathPrefix.replace(/^\/+|\/+$/g, "");
	if (prefix) {
		if (rawPath.startsWith(`${prefix}/`)) rawPath = rawPath.slice(prefix.length + 1);
		else if (!isScp) return null;
	}
	return rawPath.replace(/\.git$/i, "") || null;
}

function ticketAsEvent(ticket: GitLabTicket, projectPath: string, workspacePath: string): GitLabInboxEvent {
	return {
		id: `issue-${ticket.iid}`,
		workspacePath,
		projectPath,
		category: "assigned_issues",
		categories: ["assigned_issues"],
		entityKind: "issue",
		entityIid: ticket.iid,
		relatedIssueIid: null,
		discussionId: null,
		sessionId: null,
		title: ticket.title,
		summary: ticket.description,
		url: ticket.webUrl,
		occurredAt: ticket.updatedAt,
		readAt: null,
	};
}

function proposalSummary(proposal: GitLabProposal): string {
	const payload = recordPayload(proposal.payload);
	if (!payload) return "Review the proposed GitLab operation before approving.";
	if (proposal.kind !== "update_issue") return "Review the proposed GitLab operation before approving.";
	const original = recordPayload(payload.original);
	const changes: string[] = [];
	for (const key of ["title", "description", "labels", "state"] as const) {
		const proposed = payload[key];
		if (proposed === undefined || proposed === null) continue;
		const before = original?.[key];
		if (JSON.stringify(before) === JSON.stringify(proposed)) continue;
		changes.push(`${key}: ${displayValue(before)} → ${displayValue(proposed)}`);
	}
	if (payload.state_event === "close" || payload.state_event === "reopen") {
		changes.push(`state: ${displayValue(original?.state)} → ${payload.state_event === "close" ? "closed" : "opened"}`);
	}
	return changes.length > 0 ? changes.join(" · ") : "Review the proposed GitLab operation before approving.";
}

function recordPayload(value: unknown): Record<string, unknown> | null {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function displayValue(value: unknown): string {
	const text = Array.isArray(value) ? value.join(", ") : String(value ?? "(empty)");
	return text.length > 120 ? `${text.slice(0, 117)}…` : text;
}
