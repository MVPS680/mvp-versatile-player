---
kind: build_system
name: 基于 Cargo workspace + PowerShell 脚本的 Windows 构建与打包系统
category: build_system
scope:
    - '**'
source_files:
    - Cargo.toml
    - .cargo/config.toml
    - scripts/setup-ffmpeg.ps1
    - scripts/ffmpeg-env.ps1
    - scripts/dev.ps1
    - scripts/package.ps1
    - crates/mvp-player/build.rs
    - crates/mvp-player/Cargo.toml
    - crates/mvp-core/Cargo.toml
    - crates/mvp-platform/Cargo.toml
    - crates/mvp-subtitle/Cargo.toml
---

## 1. 使用的系统与工具

- **Cargo workspace**：根 `Cargo.toml` 通过 `[workspace]` 声明四个 crate（`mvp-core`、`mvp-platform`、`mvp-player`、`mvp-subtitle`），统一版本、edition、rust-version 和依赖。
- **FFmpeg 动态链接**：通过 `ffmpeg-next = "9.0.0"` 绑定 FFmpeg 9.0 shared 开发包，运行时 DLL 由 build script 复制到 exe 旁边。
- **Windows SDK / rc.exe**：通过 `embed-resource` crate 在 `build.rs` 中生成 `.rc` 并编译资源（图标、manifest、VERSIONINFO）。
- **PowerShell 脚本**：所有本地构建/打包流程集中在 `scripts/` 下，无 Makefile/Dockerfile/CI YAML。
- **bindgen / libclang**：`ffmpeg-sys-next` 通过 bindgen 生成 FFI 绑定，需要 `libclang.dll`。

## 2. 关键文件

- `Cargo.toml`：workspace 定义、全局依赖、release/dev profile（`opt-level=3, lto="fat", codegen-units=1, strip="symbols"`）。
- `.cargo/config.toml`：**机器本地生成**（被 `.gitignore` 忽略），记录 `FFMPEG_DIR`、`LIBCLANG_PATH`、`BINDGEN_EXTRA_CLANG_ARGS`。
- `scripts/setup-ffmpeg.ps1`：下载/校验 FFmpeg 9.0 shared kit、探测 libclang、写入 `.cargo/config.toml`。
- `scripts/ffmpeg-env.ps1`：供其他脚本 dot-source 的共享函数库（`Get-MvpFfmpegDir`、`Find-MvpLibclang`、`Test-MvpFfmpegKit` 等）。
- `scripts/dev.ps1`：包装 `cargo run/test`，自动把 FFmpeg `bin` 加到 PATH 并导出 `FFMPEG_DIR`。
- `scripts/package.ps1`：构建 release 二进制 + 复制 FFmpeg DLL + README/LICENSE/License-FFmpeg.txt 到 `dist/MVP-Versatile-Player`。
- `crates/mvp-player/build.rs`：生成 `.rc` 资源（中文语言 ID 0x0404/0x02）、嵌入 VERSIONINFO、拷贝 FFmpeg DLL 到 target 目录。
- `crates/mvp-player/Cargo.toml`：`default-run = "mvp-versatile-player"`，`[[bin]]` 指定入口，`embed-resource = "3"` 作为 build-dependency。

## 3. 架构与约定

### 3.1 依赖管理策略
- 所有 crate 使用 `version.workspace = true` 从根 `Cargo.toml` 继承版本；公共依赖集中放在 `[workspace.dependencies]`，各 crate 通过 `xxx.workspace = true` 引用。
- `mvp-player` 是唯一可执行 crate，依赖其余三个 crate（`path = "../..."`）。
- `mvp-platform` 通过 `target.'cfg(windows)'.dependencies` 条件引入 `windows` crate，仅 Windows 平台编译。

### 3.2 FFmpeg 集成模式
- 使用 **shared 开发包**（BtbN/FFmpeg-Builds 的 `win64-gpl-shared-*` zip），而非静态库。`setup-ffmpeg.ps1` 强制校验 `include/`、`lib/*.lib`、`bin/*.dll` 三者齐全（`Test-MvpFfmpegKit` 要求至少 5 个 `.lib`）。
- 链接期通过 `FFMPEG_DIR` 找到 `.lib`；运行期通过 `PATH` 或 exe 旁 DLL 加载 `.dll`。
- 固定使用 FFmpeg **n9.0 发行分支**而非 master，因为 master 头文件中已有 `ffmpeg-next 9.0.0` 不认识的编解码器枚举值，会导致编译失败。
- 通过 `BINDGEN_EXTRA_CLANG_ARGS = "-DCUarray=void* -DCUDA_ARRAY3D_DESCRIPTOR=void*"` 屏蔽 ffmpeg-sys-next 对 FFmpeg 9 CUDA 句柄类型的不兼容。

### 3.3 本机配置隔离
- `.cargo/config.toml` 由 `setup-ffmpeg.ps1` 生成，注释明确说明“本机生成的文件……已被 .gitignore 忽略”，路径不是项目属性而是机器事实。
- `dev.ps1` 会先检测该文件是否存在，不存在则自动调用 `setup-ffmpeg.ps1 -NoDownload` 提示用户准备环境。
- 所有脚本通过 dot-source `ffmpeg-env.ps1` 共享同一套路径查找逻辑，避免硬编码重复。

### 3.4 资源与版本
- `build.rs` 将 `assets/icon.ico` 和 `assets/app.manifest` 编译进 exe，语言固定为简体中文（`LANG_CHINESE_ID=0x04`, `SUBLANG_CHINESE_SIMPLIFIED_ID=0x02` → 0x0804）。
- VERSIONINFO 中的版本号取自 `CARGO_PKG_VERSION`（即 workspace 的 `1.0.2`），剥离预发布后缀后转为四段 `(u16,u16,u16,u16)`，确保 Explorer 显示与 About 对话框一致。
- `embed_resource::compile(...).manifest_required()` 使 manifest 成为必需：缺少 Windows SDK (`rc.exe`) 时构建直接 panic。

### 3.5 发布产物结构
- `scripts/package.ps1` 输出目录：`dist/MVP-Versatile-Player/`，包含 `mvp-versatile-player.exe` + 全部 FFmpeg DLL + `README.md` + `LICENSE` + `LICENSE-FFmpeg.txt`。
- 文档明确说明这是“可执行文件 + 其七个 DLL”的可分发包，无需安装程序、注册表或运行时引导。

### 3.6 Profile 优化
- `profile.release`：`opt-level=3, lto="fat", codegen-units=1, incremental=false, strip="symbols"`，注释强调保留 unwinding panic（媒体引擎 worker 线程需隔离损坏文件引发的 panic）。
- `profile.dev`：`opt-level=1`，但 `profile.dev.package."*"` 提升到 `opt-level=2`，且 `mvp-core` 单独设为 `opt-level=2`，以平衡 UI 迭代速度与核心性能。

## 4. 约定与约束

- **FFmpeg 必须是 shared 开发包**：`setup-ffmpeg.ps1` 的 `Test-MvpFfmpegKit` 校验 `include/`、`lib/`、`bin/` 存在且 `lib/*.lib` ≥ 5 个；若传入的是 static kit，会在链接阶段报 unresolved symbols。
- **FFMPEG_DIR 路径分隔符必须用正斜杠 `/`**：`setup-ffmpeg.ps1` 和 `dev.ps1` 都显式 `-replace '\\', '/'`，因为 `ffmpeg-sys-next` 声明了 `cargo:rerun-if-env-changed=FFMPEG_DIR`，反斜杠差异会触发全量 rebuild。
- **`.cargo/config.toml` 不得手动编辑**：脚本注释和实现均将其视为“本机生成的文件”，由 `setup-ffmpeg.ps1` 唯一写入；`dev.ps1` 在缺失时自动触发 setup。
- **FFmpeg 版本锁定为 9.0**：`Cargo.toml` 中 `ffmpeg-next = "9.0.0"`，`ffmpeg-env.ps1` 中 `$MvpFfmpegVersion = '9.0'`，`setup-ffmpeg.ps1` 下载 `ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip`。
- **Windows 资源语言固定为 0x0404/0x02**：`build.rs` 中常量与 `.rc` 模板保持一致，避免 `rc.exe` 因默认语言不同导致资源不可重现。
- **manifest 是必需的**：`embed_resource::compile(...).manifest_required()` 使缺少 `rc.exe` 时构建失败，而不是静默跳过。
- **发布二进制不包含 installer**：`package.ps1` 只复制 exe + DLL + 许可证文本，分发方式是“将整个目录拷贝到目标机器即可运行”。
- **测试运行需经 `dev.ps1`**：脚本注释说明 `cargo test` 需要 `<ffmpeg>/bin` 在 PATH 上，否则 FFmpeg DLL 无法在进程启动时被加载。

## 5. 未发现的 CI/容器化

仓库中没有 GitHub Actions、Azure Pipelines、Jenkinsfile、Dockerfile 或任何 CI 配置文件；构建与打包完全依赖本地 PowerShell 脚本。