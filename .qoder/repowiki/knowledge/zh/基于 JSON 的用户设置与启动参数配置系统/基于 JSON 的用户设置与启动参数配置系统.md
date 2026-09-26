---
kind: configuration_system
name: 基于 JSON 的用户设置与启动参数配置系统
category: configuration_system
scope:
    - '**'
source_files:
    - crates/mvp-player/src/settings.rs
    - crates/mvp-player/src/main.rs
    - crates/mvp-core/src/engine.rs
    - crates/mvp-platform/src/assoc.rs
---

## 1. 采用的方案

该播放器使用 **自实现的 JSON 持久化 + 命令行覆盖** 的配置体系，没有引入外部配置框架（如 `config`、`serde_yaml`、`dotenv`）。核心由 `crates/mvp-player/src/settings.rs` 中的 `Settings` 结构体承担，通过 `serde_json` 序列化为 `%APPDATA%\MVP-Versatile-Player\settings.json`；运行时启动参数通过 `main.rs` 中手写的 `parse_args` 解析为 `StartupArgs` / `LaunchOverrides`，仅影响本次会话，不写入磁盘。

## 2. 关键文件与位置

| 文件 | 职责 |
|---|---|
| `crates/mvp-player/src/settings.rs` | 用户设置的数据模型、加载/保存、迁移、去抖写入、最近文件/播放位置记忆、每文件图片调整范围 |
| `crates/mvp-player/src/main.rs` | 命令行参数解析 (`parse_args`)、日志初始化、单实例协调、窗口几何读取 |
| `crates/mvp-core/src/engine.rs` | `EngineConfig` —— 传递给底层解码引擎的运行时可调项（硬件解码、HDR 色调映射、Dolby Vision reshaping、音量/速度、队列预算等） |
| `crates/mvp-platform/src/assoc.rs` | 与 Windows 注册表交互的 `FileKinds`，用于“打开方式”和右键菜单关联 |
| `.cargo/config.toml` | Cargo 构建期配置（无额外配置键） |

## 3. 架构与设计约定

### 3.1 存储格式与位置
- 单一 JSON 文档存放所有用户偏好，路径由 `Settings::path()` 计算：`dirs::config_dir().join(APP_NAME).join("settings.json")`。
- 写入采用 **临时文件 + rename** 原子替换（`.json.tmp` → `.json`），避免崩溃损坏用户配置。
- 加载失败时原文件被重命名为 `.json.broken` 并回退到默认值，保证播放器始终可启动。
- 日志文件位于同一目录 `mvp.log`，按运行时长限制（2MB）轮转。

### 3.2 版本迁移策略
- `Settings` 包含 `version: u32` 字段（当前 `SCHEMA_VERSION = 1`），`load()` 后调用 `migrate()` 做向后兼容：例如已废弃的 `AspectMode::Source` 在迁移中被折叠为 `Fit`，旧键（如 `autoplay`）被静默忽略。
- 新增可选字段一律加 `#[serde(default)]`，确保只含部分字段的 JSON（如用户手动编辑的 `{"picture":{"brightness":0.5}}`）不会导致整份配置重置。

### 3.3 运行时层：`EngineConfig`
- `mvp_core::engine::EngineConfig` 是传给 FFmpeg 解码引擎的纯数据对象，包含硬件解码开关、HDR 色调映射、Dolby Vision reshaping、音频设备、初始音量/速度、帧队列字节/帧/秒上限、缓冲池大小、是否自动播放等。
- 这些值来自 `Settings` 并在 `Engine::new` 时固化；运行时对音量和速度的修改走 `Engine::set_volume` / `set_speed` 的原子位更新，不写回磁盘。

### 3.4 启动参数（Launch Overrides）
- 命令行支持：`--fullscreen`/`-f`、`--no-autoplay`/`--paused`、`--no-audio`/`--mute`、`--volume <值>`（支持 `0.75` 与 `75%`）、`--speed <值>`（支持 `1.5` 与 `1.5x`）、`--subtitle <文件>`、`-h/--help`、`-v/--version`，以及任意非 `--` 开头的参数作为待播放文件。
- 解析结果进入 `StartupArgs`，其中 `--volume` 与 `--speed` 同时转换为 `LaunchOverrides`，经 `Settings::apply_launch_overrides` 合并进内存 `Settings`，但在 `Settings::persisted` 中会被还原为基线值，**绝不写回 `settings.json`**。
- 未知选项直接报错退出（exit code 2），保持二进制体积最小且不依赖第三方参数解析库。

### 3.5 写入节流
- `SettingsStore` 维护 `dirty` 标记与 `last_flush`，默认 2 秒刷新一次，高频拖动滑块时避免每秒写盘数十次。
- 仅在真正有变更时才序列化；`is_dirty()` 供 UI 决定是否调用 `flush_if_due`。

### 3.6 其他平台集成点
- 单实例互斥：通过 `mvp_platform::single_instance::AppInstance` 以 `APP_ID = "mvp-versatile-player"` 保证多开时新进程把文件列表转发给主进程。
- 文件关联：`FileKinds`（video/audio/image）配合 `context_menu`、`set_as_default` 控制 Explorer 右键菜单与默认程序注册。

## 4. 约定与约束

| 规则 | 来源/证据 |
|---|---|
| 用户设置必须持久化为单个 JSON 文件，位于 `dirs::config_dir()/MVP-Versatile-Player/settings.json` | `settings.rs` 模块级注释与 `Settings::path()` |
| 写入必须原子：先写 `.tmp` 再 `rename` | `Settings::save()` 实现 |
| 损坏的设置文件必须备份为 `.json.broken` 并回退默认值 | `Settings::load()` 的 `Err` 分支 |
| 新增/废弃字段必须向后兼容：未知键忽略、缺失键用 `#[serde(default)]` | 代码注释与测试 `unknown_and_missing_fields_fall_back_to_defaults` |
| 废弃的 `AspectMode::Source` 必须在迁移中折叠为 `Fit` | `Settings::migrate()` |
| 启动参数 `--volume` / `--speed` 仅影响本次会话，不得写回配置文件 | `LaunchOverrides` 文档与 `Settings::persisted` 测试 `launch_overrides_do_not_become_preferences` |
| 未知命令行选项必须拒绝并打印帮助 | `parse_args` 中 `other if other.starts_with("--") => Err(...)` |
| 音量接受小数或百分比两种形式（>2 视为百分数） | `parse_args` 中对 `--volume` 的处理逻辑 |
| 速度必须在 0.25–4.0 范围内，且支持 `Nx` 后缀 | `parse_args` 校验与测试 `speed_is_validated` |
| 最近文件列表需去重、截断至 `recent_limit`，URL 不记录 | `push_recent` 与相关测试 |
| 每文件图片调整列表最多保留 200 项，按最近使用淘汰 | `PER_FILE_PICTURE_LIMIT` 常量与 `set_picture_for` 的 `truncate` |
| 播放位置记忆受 `remember_position` 与 `resume_min_seconds` 阈值保护，接近结尾不恢复 | `store_position` / `resume_position` 与测试 |
| 音频增强参数在送入 DSP 前必须 clamp，防止 NaN/无穷大到达着色器 | `AudioEnhanceSettings::snapshot()` 调用 `Snapshot::clamp()` |
| 日志文件单运行最大 2MB，超界则轮转为 `.log.1` | `MAX_LOG_BYTES` 与 `open_log` |
| 单实例互斥失败不应阻止播放器启动，仅降级为独立进程 | `main.rs` 中捕获错误后的 fallback 分支 |

## 5. 总结

该仓库的配置系统围绕一个 JSON 文档展开，通过 `serde` 提供强类型读写，辅以轻量级的命令行覆盖机制。它刻意避免引入重型配置框架，转而用少量手写逻辑实现原子写入、版本迁移、写入节流、启动参数隔离等工程实践，使配置既对用户可编辑、又对播放器健壮可靠。