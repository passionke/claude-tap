# tmux Real E2E Success Pattern

**Date:** 2026-02-26
**Tags:** e2e, tmux, testing, verification

## Problem

tmux `send-keys` was not reliably submitting prompts to Claude Code TUI during real
interactive E2E runs.

## Root Cause

- Some flows assumed `rg` was available for assertions, but it was missing in parts of the environment.
- Submit behavior in Claude Code TUI under tmux was mis-modeled; the correct submit key was just `Enter`.

## Solution

- Replaced fragile `rg`-based checks with portable `grep -F` checks.
- Standardized submit behavior to `Enter` by default.
- Added retry-on-miss submission logic to reduce transient input timing failures.

## Verification

Validated via JSONL assertions:

1. Both prompts appear in trace data.
2. `/v1/messages` calls are at least 2.
3. At least one response content block is `tool_use`.
4. HTML viewer artifact is generated.

## Result

- `7/7` pytest real E2E cases passed.
- tmux interactive E2E passed with confirmed `tool_use` capture.
- asciinema recording was produced.
- Browser screenshots of the generated HTML viewer were captured.

## Lesson Learned

For real Claude TUI automation under tmux, portability and input semantics matter more
than clever tooling: prefer `grep -F`, use `Enter`, and verify through trace artifacts.
