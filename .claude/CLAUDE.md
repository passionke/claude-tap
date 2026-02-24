## Pre-commit CI checks

Before every `git commit`, run these checks locally (mirrors GitHub CI):

```bash
uv run ruff check .
uv run ruff format --check .
uv run pytest tests/ -x --timeout=60
```

All three must pass before committing. If format fails, run `uv run ruff format .` first.

## Language

All code, comments, commit messages, docs, and skill files in this project must be in English. The only exceptions are `README_zh.md` and other explicitly Chinese README files.
