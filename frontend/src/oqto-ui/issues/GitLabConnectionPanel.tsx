import { useTranslation } from "react-i18next";
import { GITLAB_CATEGORIES } from "./gitlab-inbox-constants";
import type { GitLabInboxController } from "./useGitLabInboxController";

interface GitLabConnectionPanelProps {
	readonly controller: GitLabInboxController;
}

export function GitLabConnectionPanel({
	controller,
}: GitLabConnectionPanelProps) {
	const { t } = useTranslation();
	const {
		connection,
		busy,
		instanceUrl,
		setInstanceUrl,
		selectedCategories,
		setSelectedCategories,
		remoteUrl,
		setRemoteUrl,
		matchingRemotes,
		projectOverride,
		setProjectOverride,
		mappingSource,
		editingMapping,
		setEditingMapping,
		gitlabView,
		setGitlabView,
		showClosedIssues,
		setShowClosedIssues,
		refreshInbox,
		run,
		saveConnection,
		saveNotificationSettings,
		testConnection,
		saveMapping,
		removeMapping,
		disconnect,
	} = controller;

	if (!connection) {
		return (
			<section
				className="wb-issues__body"
				aria-label={t("oqtoUi.gitlab.setupLabel")}
			>
				<h3>{t("oqtoUi.gitlab.connectTitle")}</h3>
				<p>
					{t("oqtoUi.gitlab.tokenInstructions", {
						credentials: t("oqtoUi.gitlab.hostCredentials"),
					})}
				</p>
				<p>{t("oqtoUi.gitlab.personalWorkspacesOnly")}</p>
				<form onSubmit={saveConnection} className="wb-gitlab__form">
					<label>
						{t("oqtoUi.gitlab.instanceOrigin")}
						<input
							type="url"
							value={instanceUrl}
							placeholder={t("oqtoUi.gitlab.instancePlaceholder")}
							onChange={(event) => setInstanceUrl(event.target.value)}
							required
						/>
					</label>
					<fieldset>
						<legend>{t("oqtoUi.gitlab.categoriesTitle")}</legend>
						{GITLAB_CATEGORIES.map(({ id, labelKey }) => (
							<label key={id}>
								<input
									type="checkbox"
									checked={selectedCategories.includes(id)}
									onChange={(event) =>
										setSelectedCategories((current) =>
											event.target.checked
												? [...current, id]
												: current.filter((value) => value !== id),
										)
									}
								/>
								{t(`oqtoUi.gitlab.categories.${labelKey}`)}
							</label>
						))}
					</fieldset>
					<button type="submit" disabled={busy}>
						{t("oqtoUi.gitlab.connectInstance")}
					</button>
				</form>
				{controller.error && <p role="alert">{controller.error}</p>}
			</section>
		);
	}

	return (
		<>
			<div className="wb-gitlab__bar">
				<strong>
					{connection.host}
					{connection.pathPrefix}
					{connection.gitlabUsername ? ` · @${connection.gitlabUsername}` : ""}
				</strong>
				{!connection.connected ? (
					<button
						type="button"
						disabled={busy}
						onClick={() => void testConnection()}
					>
						{t("oqtoUi.gitlab.testConnection")}
					</button>
				) : (
					<button
						type="button"
						disabled={busy || !projectOverride}
						onClick={() => void run(() => refreshInbox(true))}
					>
						{t("oqtoUi.gitlab.refreshInbox")}
					</button>
				)}
				<button type="button" disabled={busy} onClick={() => void disconnect()}>
					{t("oqtoUi.gitlab.disconnect")}
				</button>
			</div>
			<details className="wb-gitlab__settings">
				<summary>{t("oqtoUi.gitlab.notificationSettings")}</summary>
				<form onSubmit={saveNotificationSettings} className="wb-gitlab__form">
					{GITLAB_CATEGORIES.map(({ id, labelKey }) => (
						<label key={id}>
							<input
								type="checkbox"
								checked={selectedCategories.includes(id)}
								onChange={(event) =>
									setSelectedCategories((current) =>
										event.target.checked
											? [...current, id]
											: current.filter((value) => value !== id),
									)
								}
							/>
							{t(`oqtoUi.gitlab.categories.${labelKey}`)}
						</label>
					))}
					<button type="submit" disabled={busy}>
						{t("oqtoUi.gitlab.saveNotifications")}
					</button>
				</form>
			</details>
			{!connection.credentialReady && (
				<p role="alert">{t("oqtoUi.gitlab.credentialMissing")}</p>
			)}
			{connection.credentialReady && !connection.connected && (
				<p role="alert">{t("oqtoUi.gitlab.testBeforePolling")}</p>
			)}
			<p className="wb-gitlab__scope">
				{t("oqtoUi.gitlab.personalWorkspacesOnly")}
			</p>
			{(!projectOverride || editingMapping) && (
				<form onSubmit={saveMapping} className="wb-gitlab__form">
					<h3>{t("oqtoUi.gitlab.mapWorkspace")}</h3>
					<label htmlFor="gitlab-remote-url">
						{t("oqtoUi.gitlab.gitRemoteAutomatic")}
						{matchingRemotes.length > 0 ? (
							<select
								id="gitlab-remote-url"
								value={remoteUrl}
								onChange={(event) => setRemoteUrl(event.target.value)}
							>
								<option value="">
									{t("oqtoUi.gitlab.selectMatchingRemote")}
								</option>
								{matchingRemotes.map((remote) => (
									<option key={remote.name} value={remote.url}>
										{remote.name}: {remote.url}
									</option>
								))}
							</select>
						) : (
							<input
								id="gitlab-remote-url"
								value={remoteUrl}
								onChange={(event) => setRemoteUrl(event.target.value)}
								placeholder={t("oqtoUi.gitlab.remotePlaceholder")}
							/>
						)}
					</label>
					<p>{t("oqtoUi.gitlab.orProjectOverride")}</p>
					<label>
						{t("oqtoUi.gitlab.projectPath")}
						<input
							value={projectOverride}
							onChange={(event) => setProjectOverride(event.target.value)}
							placeholder={t("oqtoUi.gitlab.projectPathPlaceholder")}
						/>
					</label>
					<button
						type="submit"
						disabled={busy || (!remoteUrl && !projectOverride)}
					>
						{t("oqtoUi.gitlab.saveMapping")}
					</button>
				</form>
			)}
			{projectOverride && !editingMapping && (
				<div className="wb-gitlab__bar">
					<span>
						{t("oqtoUi.gitlab.projectName")}: <code>{projectOverride}</code> ·{" "}
						{mappingSource === "override"
							? t("oqtoUi.gitlab.projectSourceOverride")
							: t("oqtoUi.gitlab.projectSourceRemote")}
					</span>
					<button
						type="button"
						onClick={() => {
							if (mappingSource === "remote") setProjectOverride("");
							setEditingMapping(true);
						}}
					>
						{t("oqtoUi.gitlab.changeMapping")}
					</button>
					<button
						type="button"
						disabled={busy}
						onClick={() => void removeMapping()}
					>
						{t("oqtoUi.gitlab.removeMapping")}
					</button>
				</div>
			)}
			{projectOverride && !editingMapping && (
				<nav
					className="wb-gitlab__views"
					aria-label={t("oqtoUi.gitlab.viewsLabel")}
				>
					<button
						type="button"
						aria-pressed={gitlabView === "issues"}
						onClick={() => setGitlabView("issues")}
					>
						{t("oqtoUi.gitlab.issuesView")}
					</button>
					<button
						type="button"
						aria-pressed={gitlabView === "inbox"}
						onClick={() => setGitlabView("inbox")}
					>
						{t("oqtoUi.gitlab.activityInbox")}
					</button>
					<button
						type="button"
						aria-pressed={showClosedIssues}
						onClick={() => setShowClosedIssues((value) => !value)}
					>
						{showClosedIssues
							? t("oqtoUi.gitlab.hideClosed")
							: t("oqtoUi.gitlab.showClosed")}
					</button>
				</nav>
			)}
		</>
	);
}
