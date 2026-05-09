"""Shared argv builders for ``python -m claude_tap`` subprocess tests.

**Use the repo's tap:** spawn with :func:`claude_tap_argv` (or at minimum
``sys.executable`` + ``-m claude_tap``), never a bare ``claude-tap`` on ``PATH``
unless you know it is this checkout. Run tests as ``uv run pytest ...`` after
``uv sync --extra dev`` so the active interpreter matches the working tree;
``tests/conftest.py`` aborts early if ``import claude_tap`` is not the repo's
``claude_tap/__init__.py``.

Subprocess tests typically write under ``tempfile.mkdtemp`` and call ``rmtree``
in ``finally``. The CLI defaults to auto-opening the HTML viewer in a
background thread, which can race deletion — so all test spawns should include
``--tap-no-open`` via :func:`claude_tap_argv`.
"""

from __future__ import annotations

import sys
from typing import Final

# Included after ``-m claude_tap`` for every :func:`claude_tap_argv` call.
TAP_SUBPROCESS_SAFE_FLAGS: Final[tuple[str, ...]] = ("--tap-no-open",)


def claude_tap_argv(
    *tap_args: str,
    python: str | None = None,
    unbuffered: bool = False,
    subprocess_safe: bool = True,
) -> list[str]:
    """Build ``[python, [-u], -m, claude_tap, [--tap-no-open], *tap_args]``.

    ``subprocess_safe`` is reserved for rare cases that must exercise auto-open
    against a non-ephemeral directory.
    """
    exe = python or sys.executable
    argv: list[str] = [exe]
    if unbuffered:
        argv.append("-u")
    argv.append("-m")
    argv.append("claude_tap")
    if subprocess_safe:
        argv.extend(TAP_SUBPROCESS_SAFE_FLAGS)
    argv.extend(tap_args)
    return argv
