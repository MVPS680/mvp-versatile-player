---
kind: logging_system
name: 基于 `log` + `env_logger` 的日志系统
category: logging_system
scope:
    - '**'
source_files:
    - Cargo.toml
    - crates/mvp-player/Cargo.toml
    - crates/mvp-core/Cargo.toml
    - crates/mvp-platform/Cargo.toml
    - crates/mvp-player/src/main.rs
    - crates/mvp-core/src/engine/workers.rs
    - crates/mvp-core/src/audio.rs
    - crates/mvp-core/src/dolby/reshape.rs
    - crates/mvp-core/src/image_view.rs
    - crates/mvp-core/src/playlist.rs
    - crates/mvp-platform/src/shell.rs
    - crates/mvp-platform/src/power.rs
    - crates/mvp-platform/src/assoc.rs
---

## 1. 使用的框架与工具

仓库采用 Rust 生态标准的 **`log` facade**（`log = "0.4"`，在 workspace 根 `Cargo.toml` 中统一声明）作为所有 crate 的日志 API，由二进制 crate `mvp-player` 通过 **`env_logger = "0.11"`** 提供具体实现。核心库 `mvp-core`、平台适配库 `mvp-platform` 仅依赖 `log` 接口；`mvp-subtitle` 完全不使用日志。

- 发布依赖：`crates/mvp-player/Cargo.toml` 引入 `log` 和 `env_logger`。
- 工作区共享版本：`Cargo.toml` `[workspace.dependencies]` 中声明 `log = "0.4"`、`env_logger = "0.11"`，被 `mvp-core`、`mvp-platform` 通过 `workspace = true` 引用。
- 测试/示例环境：`mvp-core/Cargo.toml` 将 `env_logger` 放在 `[dev-dependencies]` 下，注释说明“集成测试驱动真实播放，需要引擎自身的日志输出”。

## 2. 关键文件

- `crates/mvp-player/src/main.rs`：日志初始化与自定义 sink 的核心实现。
- `crates/mvp-core/src/engine/workers.rs`：播放引擎中最密集的日志点（debug/warn/info/error）。 
- `crates/mvp-core/src/audio.rs`、`crates/mvp-core/src/dolby/reshape.rs`、`crates/mvp-core/src/image_view.rs`、`crates/mvp-core/src/playlist.rs`：各子系统的日志调用点。
- `crates/mvp-platform/src/shell.rs`、`power.rs`、`assoc.rs`：Windows shell 集成的日志点。
- `crates/mvp-core/examples/*.rs`、`crates/mvp-core/tests/*.rs`：通过 `env_logger::builder().try_init()` 手动初始化以启用日志。

## 3. 架构与约定

### 3.1 日志级别
代码中使用 `log` crate 提供的五个宏：`log::info!`、`log::debug!`、`log::warn!`、`log::error!`。未发现 `log::trace!` 的使用。默认过滤级别为 `info`（见 `init_logging` 中的 `default_filter_or("info")`），因此 debug 级日志仅在显式设置 `RUST_LOG` 时可见。

### 3.2 初始化位置
日志只在 `mvp-player` 二进制 crate 的 `main.rs` 中初始化，函数名为 `init_logging()`。其他 crate 不初始化 logger，只消费 `log` facade。

### 3.3 输出目标（Sink）
`init_logging` 构造了一个 `env_logger::Target::Pipe`，其底层是项目内自定义的 `LazyTeeWriter`。该 writer 同时写入两个目的地：
1. **磁盘日志文件**：路径由 `log_file_path()` 返回，首次写入时才打开（lazy open），避免启动阶段因杀毒软件或网络盘扫描而阻塞。
2. **stderr**：始终同步输出到标准错误，保证 `cargo run` 和 `--help` 等开发场景可用。

### 3.4 日志轮转策略
- 常量 `MAX_LOG_BYTES = 2 * 1024 * 1024`（2 MiB）限制单次运行写入量。
- `open_log` 在文件超过上限时将原文件重命名为 `.log.1`，然后创建新文件追加写入。
- 滚动判断基于累计字节计数器而非每次 `stat`，以避免每行一次系统调用。
- 如果进程长时间运行（例如整夜播放）导致单 run 内超出预算，会触发滚动；跨 run 则在上次 run 的文件超限时才滚动。

### 3.5 线程安全
`LazyTeeWriter` 内部用 `Mutex<Option<LogFile>>` 保护文件句柄，确保多线程并发写入时的互斥访问。若 mutex 被 poisoned 或文件打开失败，writer 回退为仅写 stderr，保证日志不会阻塞主流程。

### 3.6 时间戳格式
通过 `builder.format_timestamp_millis()` 启用毫秒级时间戳。

### 3.7 环境变量控制
通过 `env_logger::Env::default().default_filter_or("info")` 读取 `RUST_LOG` 环境变量来调整日志级别，未设置时默认 `info`。

## 4. 约定与约束

- **API 抽象层**：业务 crate（`mvp-core`、`mvp-platform`）仅依赖 `log` facade，不直接依赖 `env_logger`；具体后端由 `mvp-player` 二进制 crate 注入。这是通过 Cargo 依赖关系强制实现的约束。
- **日志级别使用约定**：`workers.rs` 中大量 `log::debug!` 用于追踪解码帧、seek 过程等高频信息；`log::warn!` 用于可恢复异常（如音频设备不可用、字幕解码器打开失败、跳转失败后继续播放）；`log::error!` 用于严重错误；`log::info!` 用于关键状态变更（硬件解码器启用、音频设备信息等）。
- **无结构化字段**：日志消息全部使用格式化字符串（如 `log::warn!("无法打开内嵌字幕解码器: {err}")`），未使用 `tracing` 风格的键值对结构化字段。
- **语言**：日志消息使用中文为主（如“音频输出错误”、“杜比视界映射方式 {other} 未知，未应用重塑”），部分 Windows API 相关日志保留英文（如 `SetCurrentProcessExplicitAppUserModelID({id:?}) failed`）。
- **测试环境需手动初始化**：`mvp-core` 的 tests 和 examples 必须自行调用 `env_logger::builder().try_init()` 才能看到日志，因为 `env_logger` 不在它们的运行时依赖中。
- **日志文件命名与位置**：由 `log_file_path()` 决定（未在已读片段中展示），但轮转后缀固定为 `.log.1`。
- **性能约束**：日志文件 lazy open、按字节计数而非 stat 判断滚动、mutex 保护——这些设计明确针对“反病毒扫描和网络配置文件导致的 I/O 延迟”以及“长时间运行的播放器不能无限增长日志文件”的场景（见 `init_logging` 与 `LazyTeeWriter` 的注释）。