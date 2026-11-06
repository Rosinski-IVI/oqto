import crypto from "node:crypto";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

const RPC_TITLE = "OQTO_GITLAB_RPC_V1";

const GitLabOperationParams = Type.Object(
	{
		operation: Type.String({ description: "One supported Oqto GitLab operation listed in the tool description." }),
		title: Type.Optional(Type.String()),
		description: Type.Optional(Type.String()),
		labels: Type.Optional(Type.Array(Type.String(), { maxItems: 50 })),
		state_event: Type.Optional(Type.Union([Type.Literal("close"), Type.Literal("reopen")])),
		body: Type.Optional(Type.String()),
		discussion_id: Type.Optional(Type.String()),
		comments: Type.Optional(Type.Array(Type.Object({
			path: Type.String(),
			line: Type.Integer({ minimum: 1 }),
			side: Type.Union([Type.Literal("old"), Type.Literal("new")]),
			body: Type.String(),
		}, { additionalProperties: false }), { maxItems: 50 })),
		remote_name: Type.Optional(Type.String()),
		target_branch: Type.Optional(Type.String()),
	},
	{ additionalProperties: false },
);

type ToolParams = {
	operation: string;
	title?: string;
	description?: string;
	labels?: string[];
	state_event?: "close" | "reopen";
	body?: string;
	discussion_id?: string;
	comments?: { path: string; line: number; side: "old" | "new"; body: string }[];
	remote_name?: string;
	target_branch?: string;
};

function operationFromParams(params: ToolParams): Record<string, unknown> {
	const optional = (key: keyof ToolParams) =>
		params[key] === undefined ? {} : { [key]: params[key] };
	switch (params.operation) {
		case "read_selected_issue":
		case "read_selected_issue_notes":
		case "read_linked_merge_request":
		case "read_linked_merge_request_discussions":
		case "read_project_context":
		case "propose_implementation_start":
			return { operation: params.operation };
		case "propose_issue_update":
			return {
				operation: params.operation,
				...optional("title"),
				...optional("description"),
				...optional("labels"),
				...optional("state_event"),
			};
		case "propose_issue_comment":
			return { operation: params.operation, ...optional("body") };
		case "propose_discussion_reply":
			return {
				operation: params.operation,
				...optional("discussion_id"),
				...optional("body"),
			};
		case "propose_inline_review":
			return { operation: params.operation, ...optional("comments") };
		case "create_or_update_draft":
			return {
				operation: params.operation,
				...optional("remote_name"),
				...optional("title"),
				...optional("description"),
				...optional("target_branch"),
			};
		default:
			throw new Error(`Unsupported Oqto GitLab operation: ${params.operation}`);
	}
}

export default function register(pi: ExtensionAPI) {
	pi.registerTool({
		name: "oqto_gitlab",
		label: "Oqto GitLab",
		description: [
			"Use Oqto's session-bound GitLab integration for the issue selected in this session.",
			"Supported operations: read_selected_issue, read_selected_issue_notes, read_linked_merge_request, read_linked_merge_request_discussions, read_project_context, propose_implementation_start, propose_issue_update, propose_issue_comment, propose_discussion_reply, propose_inline_review, create_or_update_draft.",
			"Reads are limited to the selected ticket and its Oqto-linked merge request. All ticket changes, comments, discussion replies, reviews, and implementation starts create approval proposals; you cannot approve them.",
			"Draft MR creation/update is allowed only after implementation approval. It pushes the current committed feature branch with normal Git auth; it never force-pushes. If origin is not a matching remote, ask the user which listed matching remote to use. A ready MR cannot be re-drafted by this tool; ask the user to confirm through Oqto.",
		].join("\n"),
		parameters: GitLabOperationParams,
		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			const requestId = crypto.randomUUID();
			const envelope = {
				version: 1,
				request_id: requestId,
				operation: operationFromParams(params as ToolParams),
			};
			// Oqto intercepts this typed Pi input request on the active runner
			// subscription and returns a response through Pi's existing UI RPC.
			// No URL, bearer token, credential reference, or workspace is sent.
			const raw = await ctx.ui.input(RPC_TITLE, JSON.stringify(envelope), {
				timeout: 600_000,
				...(signal ? { signal } : {}),
			});
			if (typeof raw !== "string" || raw.length > 128 * 1024) {
				throw new Error("Oqto GitLab RPC was cancelled or returned an oversized response.");
			}
			const response = JSON.parse(raw) as {
				version?: unknown;
				request_id?: unknown;
				ok?: unknown;
				result?: unknown;
				error?: unknown;
			};
			if (response.version !== 1 || response.request_id !== requestId) {
				throw new Error("Oqto GitLab RPC response identity mismatch.");
			}
			if (response.ok !== true) {
				throw new Error(typeof response.error === "string" ? response.error : "Oqto GitLab operation failed.");
			}
			const text = JSON.stringify(response.result ?? null, null, 2);
			return {
				content: [{ type: "text", text }],
				details: { operation: params.operation, result: response.result },
			};
		},
	});
}
