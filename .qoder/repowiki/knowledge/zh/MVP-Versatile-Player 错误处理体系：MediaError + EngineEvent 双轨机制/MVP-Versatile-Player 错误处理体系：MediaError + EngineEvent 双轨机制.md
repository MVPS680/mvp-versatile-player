---
kind: error_handling
name: MVP-Versatile-Player 错误处理体系：MediaError + EngineEvent 双轨机制
category: error_handling
scope:
    - '**'
source_files:
    - crates/mvp-core/src/error.rs
    - crates/mvp-core/src/lib.rs
    - crates/mvp-core/src/engine.rs
    - crates/mvp-core/src/engine/workers.rs
    - crates/mvp-platform/src/shell.rs
    - crates/mvp-platform/src/single_instance.rs
---

## 1. 总体方案

仓库采用 **分层错误模型**：

- `mvp-core`（媒体引擎）使用自有的 `MediaError` 枚举，通过 `thiserror::Error` 派生，作为所有解码/渲染路径的返回类型。
- 跨线程的错误以 `EngineEvent::Error(String)` 和 `PlaybackState::Error(String)` 两种形式异步上报给 UI。
- `mvp-platform`（平台适配层）统一暴露 `anyhow::Result`，将 Windows-only 与跨平台实现解耦。
- 顶层 `mvp-player` 应用负责把底层错误翻译为对话框、日志或设置项。

核心设计文档在 `crates/mvp-core/src/lib.rs` 中声明："Panic-proof. Each worker thread catches panics and turns them into an error state, because damaged files are a fact of life." —— 这是该仓库对错误处理的明确约定。