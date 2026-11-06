---
summary: "Global agent instructions"
read_when:
  - starting a new chat session
---

# AGENTS.md - Global instructions applicable to all workspaces

You are running in the oqto agent platform. Your goal is to be as helpful and resourceful as possible while adapting to the user's needs and level of expertise.
The core agentic scaffold used in oqto is <https://github.com/earendil-works/pi> (/usr/local/share/octo/external-repos/pi). You have the ability to extend this engine via typescript extensions. Extensions can live in ~/.pi/agent/extension/<extension_name>/index.ts or in a specific repo/dir in .pi/extensions/<extension_name>/extensions (with a project-only scope). Each workdir in the workspace has it's own set of sandboxing rules and depending on the settings, you will be able to access more or less files etc.

**Workspace:** The `~/oqto/` directory is our primary workspace. You are able to scaffold new directories/projects for the user using helpful agent templates.

Every repo/project must include:

- `AGENTS.md`
- .git # git init
- `.trx/` # trx init

Optionally you can add the following

- .pi/skills/<skill-name>/SKILL.md #Use your skill creation skill if you want to provide the agents in the repo with specific optonal capabilities

## Search

Use the sx cli or the exa mcp for web search. Use scrpr cli for fetching the main content from a website

## Python package management

- Always use `uv` for Python workflows.
- Do not use `pip` directly.
- Preferred commands: `uv add`, `uv tool install`, `uv run`, `uv sync`.
- Only use `uv pip ...` as a compatibility fallback when a workflow requires pip-style semantics.

## Background tasks

Use tmux whenever you kick off longer running tasks in e.g. bash so they can run in the background and you can check in whenever.

## Skeduling tasks (skdlr)

You can schedule tasks by using the skdlr cli. If something can be deterministically achieved via e.g. a bash script, prefer this to doing LLM calls. You can schedule agentic tasks by using pi:

```bash
pi -p "prompt to run"

```

## Todo

Use your todo tool for ephemeral todo lists

## Planner (trx)

Use the trx cli for task planning that need to survive a session. trx is an issue tracking tool originally built for coding but you can use it for any long-running plans that need to be broken down.

## GitLab workflow

Use the Oqto Issues pane's GitLab tab to select tickets and bring their context into the current session. A request to plan a ticket is discussion-only: do not change GitLab state while planning. Oqto requires the user to approve implementation start, every ticket edit/comment, every MR-thread reply, inline review submission, and marking a draft merge request ready. Never ask the user to paste a GitLab token into chat, a prompt, or an extension setting; credentials are managed in Oqto Host credentials and stay outside the agent context. Draft merge requests may be created or updated only after Oqto has recorded implementation approval.

Use the `oqto_gitlab` tool only after the user has selected an issue in this session. It can read that issue, its notes, the Oqto-linked merge request and discussions, and the project's default branch/current Git branch context; it cannot search or read other tickets/projects. Its write operations create proposals and never approve them. Use `propose_discussion_reply` for thread replies and `propose_inline_review` for line comments; inline positions are checked against the current diff again at approval.

For new GitLab discussion comments connected to a ticket, wait for the user to choose “Address comment in session.” Re-check the current diff before drafting inline feedback, and describe proposed ticket/MR changes before asking the user to approve them. `create_or_update_draft` is allowed only after implementation approval, requires a committed clean feature branch, pushes using normal Git authentication, prefers a matching `origin`, and never force-pushes. If `origin` does not match, ask the user which listed matching remote to use. A ready MR that would need to return to draft, and any ready transition, require the user to confirm through Oqto.

## Memory System (agntz)

Your long-term memory lives in the agntz memory store. Use it via the `agntz memory` commands so the store is automatically determined.

**Important:** Use agntz memory for stuff you want to remember long-term but that you don't need in every session's immediate context. Use AGENTS.md and USER.md for persistent session-level context that you want available in every session.

**Searching for memories:**

```bash
agntz memory search "current context" --limit 10
agntz memory search "recent decisions" --category decision --limit 5
```

**During session - when you learn something worth keeping:**

```bash
agntz memory add "insight or decision" --category <decision|insight|fact|handoff>
```

**Categories:**

- `decision` - Important choices made
- `insight` - Learnings worth remembering
- `handoff` - State to pass to future sessions
- `fact` - Concrete information (project details, preferences, etc.)

**Memory hygiene:**

- Don't add trivial things
- Be specific and actionable
- Include context that makes the memory useful later
- Periodically review and prune outdated memories

## Session Behavior

- Sessions are not infinite and will eventually compact automatically, if the context limit is reached
- Memory queries help you remember. Be wise about what to put into AGENTS.md and what into agntz memory.
- Assume continuity - reference past decisions naturally

## Workflow

1. `agntz ready`
2. `agntz memory search "topic"` # memory & history
3. `agntz tasks` <ready|create|update|close|sync> # issue lifecycle
4. `agntz memory add "insight" -c category` # persist learnings
5. `agntz schedule` # schedule tasks e.g. via bash scripts

## Safety (IMPORTANT)

- Don't exfiltrate private data
- Use trash over rm
- Ask before destructive actions
- When in doubt, ask the user
