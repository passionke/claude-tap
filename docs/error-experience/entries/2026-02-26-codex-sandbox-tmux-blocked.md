# Codex Sandbox Blocked tmux Socket Creation

**Date:** 2026-02-26
**Severity:** Medium
**Tags:** codex, sandbox, tmux, environment

## Problem

Running tmux-based E2E flows inside Codex `--full-auto` failed because tmux could
not create or access its socket path under `/private/tmp` (permission denied).

## Impact

- Codex could update code and docs but could not execute tmux interactive tests directly.
- End-to-end validation requiring tmux had to be completed outside the Codex sandbox.

## Workaround

- Use Codex for code edits, refactors, and static/testable logic.
- Run tmux-dependent validation outside Codex (for example, via OpenClaw exec or local shell).
- Feed results back into repo docs/tests after external verification.

## Lesson Learned

Tasks requiring system-level terminal multiplexers (`tmux`, `screen`) are not fully
delegable to a restricted Codex sandbox. Split responsibilities explicitly between
sandbox-safe work and external execution.
