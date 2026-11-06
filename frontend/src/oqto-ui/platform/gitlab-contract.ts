import type { GitLabConnectionHost } from "./gitlab-connection-contract";
import type { GitLabInboxHost } from "./gitlab-inbox-contract";
import type { GitLabMappingHost } from "./gitlab-mapping-contract";
import type { GitLabMergeRequestHost } from "./gitlab-merge-request-contract";
import type { GitLabProposalHost } from "./gitlab-proposal-contract";

export type {
	GitLabConnection,
	GitLabEventCategory,
	GitLabInboxEvent,
	GitLabProposal,
	GitLabProposalDraft,
	GitLabProjectMapping,
	GitLabTicket,
	GitLabTicketList,
	GitLabUnreadCounts,
} from "./gitlab-types";

/** GitLab operations grouped by capability to keep each public boundary small. */
export type GitLabHost = GitLabConnectionHost &
	GitLabMappingHost &
	GitLabInboxHost &
	GitLabProposalHost &
	GitLabMergeRequestHost;
