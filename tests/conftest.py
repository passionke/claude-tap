"""Pytest configuration and shared fixtures."""

import shutil
import tempfile
from pathlib import Path

import pytest

from claude_tap.session_dispatcher import SessionTraceDispatcher
from claude_tap.session_index import SessionIndex


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
    return Path(__file__).parent.parent
