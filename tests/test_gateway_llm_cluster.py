"""Tests for per-cluster PostgreSQL LLM loading. Author: kejiqing"""

from claude_tap.gateway_llm import _runtime_from_revision, normalize_upstream_base_url


def test_runtime_from_revision_maxiot_url():
    rt = _runtime_from_revision(
        model_id="llm-1",
        model_rev="2026-05-29_16-17-39",
        base_model_url="https://llm-gw-sh.maxiot-inc.com:5443/v1",
        model_name="qwen3.7-max",
    )
    assert rt is not None
    assert rt.base_model_url == "https://llm-gw-sh.maxiot-inc.com:5443/v1"
    assert rt.model_name == "qwen3.7-max"


def test_normalize_upstream_strips_trailing_slash():
    assert normalize_upstream_base_url("https://api.deepseek.com/") == "https://api.deepseek.com"
