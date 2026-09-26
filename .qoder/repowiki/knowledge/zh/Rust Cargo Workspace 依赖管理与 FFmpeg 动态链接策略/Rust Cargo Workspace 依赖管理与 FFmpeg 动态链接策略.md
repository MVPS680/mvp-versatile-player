---
kind: dependency_management
name: Rust Cargo Workspace 依赖管理与 FFmpeg 动态链接策略
category: dependency_management
scope:
    - '**'
source_files:
    - Cargo.toml
    - Cargo.lock
    - .cargo/config.toml
    - crates/mvp-core/Cargo.toml
    - crates/mvp-platform/Cargo.toml
    - crates/mvp-player/Cargo.toml
    - crates/mvp-subtitle/Cargo.toml
    - scripts/setup-ffmpeg.ps1
---

## 1. 使用的系统/方法

本项目采用 **Cargo workspace**（resolver = "2"）组织四个 crate：`mvp-core`、`mvp-platform`、`mvp-player`、`mvp-subtitle`。所有第三方依赖通过根 `Cargo.toml` 的 `[workspace.dependencies]` 段集中声明，各子 crate 仅以 `xxx.workspace = true` 引用，确保版本统一。

- 版本锁定：仓库根目录存在 `Cargo.lock`，由 Cargo 自动生成并纳入版本控制，保证全仓构建可重现。
- 平台特定依赖：使用 `target.'cfg(windows)'.dependencies` 仅在 Windows 目标引入 `windows` crate 及其大量 `Win32_*` feature，避免跨平台编译时拉入无关依赖。
- 可选特性：`mvp-platform` 将 `serde` 设为 optional + default feature，下游可按需关闭。

## 2. 关键文件与包

| 文件 | 作用 |
|---|---|
| `Cargo.toml`（根） | workspace 定义、`[workspace.dependencies]` 集中声明所有三方库版本、全局 release/dev profile |
| `Cargo.lock` | 精确锁定所有依赖树（含 transitive deps），提交至仓库 |
| `.cargo/config.toml` | 本机 FFmpeg 路径、LIBCLANG_PATH、BINDGEN_EXTRA_CLANG_ARGS；由 `scripts/setup-ffmpeg.ps1` 生成并被 `.gitignore` 忽略 |
| `crates/*/Cargo.toml` | 各 crate 仅引用 workspace 依赖与本地 path 依赖（如 `mvp-core = { path = "../mvp-subtitle" }`） |
| `scripts/setup-ffmpeg.ps1` | 自动下载/解压 FFmpeg 9.0 shared dev 包并写入 `.cargo/config.toml` |

主要第三方依赖分组：
- FFI / 媒体：`ffmpeg-next = "9.0.0"`、`cpal = "0.16"`、`image = "0.25"`
- GUI：`eframe = "0.32"`（禁用 `default-features`，仅启用 `glow`）、`egui = "0.32"`、`rfd = "0.15"`
- 通用：`anyhow`、`thiserror`、`log`/`env_logger`、`parking_lot`、`crossbeam-channel`、`bitflags`、`once_cell`、`serde`、`dirs`、`encoding_rs`、`walkdir`、`bytemuck`、`windows`
- build-dependencies：`embed-resource = "3"`（嵌入 Windows 资源）

## 3. 架构与约定

- **集中式版本管理**：所有 crate 的 `version`、`edition`、`license` 通过 `[workspace.package]` 单点维护，子 crate 用 `version.workspace = true` 继承。
- **FFmpeg 动态链接**：不使用静态链接或 vendoring。运行时依赖系统上已安装的 FFmpeg 9.0 shared 开发包（`include/`、`lib/`、`bin/` 三目录）。`.cargo/config.toml` 中 `FFMPEG_DIR` 指向该路径，`force = false` 允许环境变量覆盖。
- **bindgen 补丁**：因 `ffmpeg-sys-next` 尚未支持 FFmpeg 9 新增的 CUDA 句柄类型，通过 `BINDGEN_EXTRA_CLANG_ARGS = "-DCUarray=void* -DCUDA_ARRAY3D_DESCRIPTOR=void*"` 在编译期屏蔽不兼容符号，注释明确“播放器本身不碰 CUDA”。
- **GUI 字体策略**：`eframe` 与 `egui` 均关闭 `default-features`，禁止自带字体打包，改为运行时安装 Windows 系统字体栈（见注释 `theme::install_fonts`），避免 CJK 回退字体冲突与二进制膨胀。
- **发布优化**：根 `profile.release` 开启 `lto = "fat"`、`codegen-units = 1`、`strip = "symbols"`、`opt-level = 3`；dev 下依赖 crate 仍保持 `opt-level = 2` 以保证 UI 迭代速度。

## 4. 约定与约束

- **依赖版本必须通过 `[workspace.dependencies]` 声明**：各 crate 不得直接写版本号，只能 `xxx.workspace = true`，由根文件统一管理。
- **FFmpeg 版本绑定**：`.cargo/config.toml` 注释明确要求 FFmpeg 使用 9.0 发行分支，与 `ffmpeg-next = "9.0.0"` 严格对应；master 分支包含新编解码器枚举，不被当前 crate 识别。
- **本机配置不入库**：`.cargo/config.toml` 顶部注释说明其为“本机生成的文件”，已被 `.gitignore` 忽略；换机器只需重跑 `setup-ffmpeg.ps1`。
- **Windows 专属依赖隔离**：`mvp-platform` 使用 `target.'cfg(windows)'.dependencies` 限定 `windows` crate 仅在 Windows 目标生效，feature 列表精确到所需 Win32 API 组。
- **panic 行为保留**：release profile 未设置 `panic = "abort"`，注释说明“媒体引擎运行在工作线程上，必须能 containment 损坏文件触发的 panic，而不拖垮整个播放器”。
- **无私有注册表/Vendoring**：未发现 `Cargo.toml` 中的 `[registries]`、`[source]` 重写、`vendor/` 目录或 `--frozen` CI 标志；所有依赖来自 crates.io 与系统 FFmpeg。