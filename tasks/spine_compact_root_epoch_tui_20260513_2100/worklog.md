# Spine Compact Root Epoch/TUI 修复 Worklog

## 2026-05-13

- 接手未完成 Plan3/compact/root epoch/TUI 修复。
- 当前目标：固定 hidden real root 不参与业务编号；用户可见和内部持久化编号均去掉不变的 `1.` 前缀；root epoch 直接编号为 `1`, `2`, `3`，root compact 后进入 `N.1`；连续 compact 不留下 orphan tool output；close/next compact 中断对齐 auto-compact。
- 并发风险：2026-05-13 本轮执行时 `state.rs` 曾被其他 Codex 进程反复改动，导致 ids/view/runtime 与 state/store 语义互相覆盖。最终收敛为单一方案：`NodeId::root()` 仅作隐藏 sentinel，不作为普通节点持久化；`1`, `2`, `3` 是 root epochs；`1.1`, `2.1`, `3.1` 是对应 epoch 的默认 live leaf。
- 验证：新增连续 root compact 回归测试，覆盖 tree tool call/output 跨 compact 边界时不会留下 orphan `FunctionCallOutput`；`cargo test -p codex-core --lib spine -- --nocapture` 通过 157 项；`cargo check -p codex-core --tests` 通过。
