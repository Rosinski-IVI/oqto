import { createLiveGitLabHost } from "@/src/oqto-ui/platform/live-gitlab";
import { afterEach, describe, expect, it, vi } from "vitest";

describe("GitLab platform adapter", () => {
	afterEach(() => vi.unstubAllGlobals());

	it("keeps the GitLab token outside browser requests", async () => {
		const fetchMock = vi.fn(
			async (_input: RequestInfo | URL, _init?: RequestInit) =>
				({
					ok: true,
					status: 200,
					json: async () => ({
						host: "gitlab.example",
						path_prefix: "",
						credential_id: "gitlab",
						revision: 1,
						enabled: true,
						event_categories: ["assigned_issues"],
						credential_ready: true,
						connected: false,
						gitlab_username: null,
					}),
				}) as Response,
		);
		vi.stubGlobal("fetch", fetchMock);

		const connection = await createLiveGitLabHost().saveConfig({
			instanceUrl: "https://gitlab.example",
			enabled: true,
			eventCategories: ["assigned_issues"],
		});
		expect(connection.credentialReady).toBe(true);
		const request = fetchMock.mock.calls[0]?.[1];
		expect(request?.credentials).toBe("include");
		expect(request?.headers).not.toHaveProperty("PRIVATE-TOKEN");
		expect(request?.headers).not.toHaveProperty("Authorization");
		expect(String(request?.body)).not.toContain("gitlab-token-value");
	});
});
