@AGENTS.md
@docs/agent-reference.md

# Claude Code configuration

Project-specific Claude subagents are available under `.claude/agents/`. Shared skills are exposed
through `.claude/skills`, which is a relative symlink to the canonical `.agents/skills/` tree.
Claude-only hooks and the Rust analyzer plugin are configured in `.claude/settings.json`.
