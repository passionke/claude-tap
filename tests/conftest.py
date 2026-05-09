"""Pytest configuration and shared fixtures."""

import os
import shutil
import tempfile
from pathlib import Path

import pytest

from claude_tap.session_dispatcher import SessionTraceDispatcher
from claude_tap.session_index import SessionIndex

_REPO_ROOT = Path(__file__).resolve().parent.parent


def pytest_configure(_config: pytest.Config) -> None:
    """Fail fast when tests are not using the working-tree ``claude_tap`` package.

    Subprocess E2E tests spawn ``sys.executable -m claude_tap``; that only tracks
    repo code if this interpreter loads the repo package (typically ``uv sync`` /
    ``uv run pytest`` from the clone). Skip this check with
    ``CLAUDE_TAP_TEST_ALLOW_FOREIGN_INSTALL=1`` for rare downstream packaging
    experiments.
    """
    if os.environ.get("CLAUDE_TAP_TEST_ALLOW_FOREIGN_INSTALL") == "1":
        return
    import claude_tap

    repo_init = _REPO_ROOT / "claude_tap" / "__init__.py"
    loaded = Path(claude_tap.__file__).resolve()
    if not repo_init.is_file():
        return
    try:
        same = loaded.samefile(repo_init)
    except OSError:
        same = loaded == repo_init
    if not same:
        pytest.exit(
            f"Tests must load claude_tap from this repo ({repo_init}), "
            f"but `import claude_tap` resolved to {loaded}. "
            "Use: `uv sync --extra dev` then `uv run pytest tests/ ...` from the repo root. "
            "(Override: CLAUDE_TAP_TEST_ALLOW_FOREIGN_INSTALL=1)",
            returncode=4,
        )


def make_trace_dispatcher(output_dir: Path) -> SessionTraceDispatcher:
    """Build a dispatcher with SQLite index under ``output_dir`` (directory is created)."""
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    return SessionTraceDispatcher(output_dir, SessionIndex(output_dir), live_server=None)


@pytest.fixture
def temp_trace_dir():
    """Create a temporary directory for trace output."""
    trace_dir = tempfile.mkdtemp(prefix="claude_tap_test_")
    yield trace_dir
    shutil.rmtree(trace_dir, ignore_errors=True)


@pytest.fixture
def temp_bin_dir():
    """Create a temporary directory for fake binaries."""
    bin_dir = tempfile.mkdtemp(prefix="claude_tap_bin_")
    yield bin_dir
    shutil.rmtree(bin_dir, ignore_errors=True)


@pytest.fixture
def project_dir():
    """Return the project root directory."""
    return _REPO_ROOT
