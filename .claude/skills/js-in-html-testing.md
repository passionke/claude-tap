---
name: js-in-html-testing
description: Test JS logic embedded in HTML using two-layer strategy - Python unit tests + Playwright browser integration tests
user_invocable: false
---

# JS-in-HTML 分层测试策略

对于嵌入在 HTML 中的 JavaScript 逻辑（如 viewer.html 的 diff 导航），分两层测试。

## Layer 1: Python 单元测试（快速验证算法）

用 Python 复刻 JS 核心算法，在 pytest 中快速验证逻辑正确性。

适用于：纯计算逻辑、状态判断、匹配算法等不依赖 DOM 的函数。

**示例**：`tests/test_diff_matching.py`

```python
# 复刻 JS 的 findPrevSameModel / findNextSameModel
def find_diff_parent_by_prefix(entries, idx):
    ...

def find_next_by_prefix(entries, idx):
    ...

# 复刻 JS 的 updateNavButtons 状态计算
def compute_nav_button_states(entries, cur_idx):
    prev_idx = find_diff_parent_by_prefix(entries, cur_idx)
    ...
    return (prev_enabled, next_enabled)
```

优点：跑得快（0.02s）、不依赖浏览器、和项目 pytest 体系集成。

## Layer 2: Playwright 浏览器集成测试（验证 DOM 交互）

生成包含测试数据的 HTML，用 Playwright 在真实 Chromium 中验证 DOM 状态和用户交互。

适用于：按钮 disabled 状态、overlay 显示/隐藏、键盘事件、click 导航等。

**示例**：`tests/test_nav_browser.py`

### 构建测试 HTML

```python
def _build_test_html():
    template = Path("claude_tap/viewer.html").read_text()
    records = [json.dumps(e) for e in TEST_ENTRIES]
    data_js = "const EMBEDDED_TRACE_DATA = [\n" + ",\n".join(records) + "\n];\n"
    # 注入数据到模板
    return template.replace(
        "<script>\nconst $ = s =>",
        f"<script>\n{data_js}</script>\n<script>\nconst $ = s =>",
        1,
    )
```

### Playwright fixture

```python
@pytest.fixture(scope="module")
def browser_page(html_file):
    from playwright.sync_api import sync_playwright
    pw = sync_playwright().start()
    browser = pw.chromium.launch(headless=True)
    page = browser.new_page()
    page.goto(f"file://{html_file}")
    page.wait_for_selector(".sidebar-item", timeout=5000)
    yield page
    browser.close()
    pw.stop()
```

### 验证 DOM 状态

```python
def _get_nav_state(page):
    return page.evaluate("""() => {
        const overlay = document.querySelector('.diff-overlay');
        if (!overlay) return { overlayExists: false };
        return {
            overlayExists: true,
            prevDisabled: overlay.querySelector('.diff-nav-prev')?.disabled,
            nextDisabled: overlay.querySelector('.diff-nav-next')?.disabled,
            title: overlay.querySelector('.diff-title')?.textContent,
        };
    }""")
```

### 模拟用户交互

```python
# 点击按钮
page.click(".diff-nav-next")
# 键盘导航
page.keyboard.press("ArrowRight")
# 通过 JS 调用内部函数
page.evaluate("selectEntry(2)")
page.evaluate("showDiff()")
```

## 注意事项

- trace 数据中可能包含 `</script>` 文本（Claude 讨论代码时），会破坏 `<script>` 块解析，需转义：`line.replace("</script>", '</scr" + "ipt>')`
- 测试数据需匹配 viewer 预期的 JSONL 格式（含 `turn`、`duration_ms`、`request_id`、`request.path` 等字段）
- Playwright 需要 `uv pip install playwright` 安装

## 运行

```bash
# 快速单元测试
uv run pytest tests/test_diff_matching.py -v

# 浏览器集成测试
uv run pytest tests/test_nav_browser.py -v

# 全部
uv run pytest tests/ --ignore=tests/test_e2e.py -v
```
