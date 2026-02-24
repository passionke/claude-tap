---
name: playwright-screen-recording
description: Record browser test videos with Playwright for PR review and bug fix verification
user_invocable: false
---

# Playwright 录屏验证

用 Playwright 的 video recording 功能录制 headless 浏览器操作视频，用于 PR review 展示或 bug 修复验证。

## 核心用法

```python
from playwright.sync_api import sync_playwright
import tempfile
from pathlib import Path

video_dir = Path(tempfile.mkdtemp())

with sync_playwright() as pw:
    browser = pw.chromium.launch(headless=True)
    context = browser.new_context(
        viewport={"width": 1400, "height": 900},
        record_video_dir=str(video_dir),
        record_video_size={"width": 1400, "height": 900},
    )
    page = context.new_page()
    page.goto(f"file:///path/to/test.html")

    # ... 执行测试操作，加 wait_for_timeout 让录屏可读 ...
    page.wait_for_timeout(800)  # 停顿让观看者看清当前状态

    page.close()
    context.close()  # 视频在 context.close() 后才写入完成
    browser.close()

# 取出录制的视频
videos = list(video_dir.glob("*.webm"))
if videos:
    videos[0].rename("demo.webm")
```

## 适用场景

- **Bug 修复验证**：录制修复前后的对比操作，展示按钮状态变化、UI 行为差异
- **PR Review**：附上 .webm 视频让 reviewer 直观理解改动效果
- **回归测试证据**：录制关键交互路径，作为测试通过的可视化记录

## 录屏技巧

### 操作间加停顿

```python
page.click(".some-button")
page.wait_for_timeout(800)   # 让观看者看清按钮点击效果

page.keyboard.press("ArrowRight")
page.wait_for_timeout(600)   # 让观看者看清键盘导航结果
```

### 同时验证 + 输出日志

```python
state = get_nav_state(page)
print(f"[1] Title: {state['title']}")
print(f"    prev disabled: {state['prevDisabled']}  (expected: True)")
assert state["prevDisabled"] is True
print("    PASS")
```

终端输出和录屏视频配合，提供双重验证。

### 使用真实数据

优先用项目已有的真实 trace 数据而非合成数据，更有说服力：

```python
# 从真实 trace 文件构建测试 HTML
records = []
with open(".traces/trace_xxx.jsonl") as f:
    for line in f:
        # 转义 </script> 防止破坏 HTML
        records.append(line.strip().replace("</script>", '</scr" + "ipt>'))
```

## 注意事项

- 视频格式为 `.webm`（VP8 编码），大多数播放器和浏览器都支持
- 每个 `page` 生成独立的视频文件
- `record_video_size` 控制视频分辨率，建议和 `viewport` 保持一致
- headless 模式下录屏正常工作，无需显示器
- 视频文件通常几百 KB，适合附到 PR 或 IM 中
