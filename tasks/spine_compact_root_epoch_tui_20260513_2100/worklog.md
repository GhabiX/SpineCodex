# Spine Compact Root Epoch/TUI 修复 Worklog

## 2026-05-13

- 接手未完成 Plan3/compact/root epoch/TUI 修复。
- 当前目标：固定 hidden real root，用户可见编号去掉 `1.`；root compact 后进入 `N.1`；连续 compact 不留下 orphan tool output；close/next compact 中断对齐 auto-compact。
