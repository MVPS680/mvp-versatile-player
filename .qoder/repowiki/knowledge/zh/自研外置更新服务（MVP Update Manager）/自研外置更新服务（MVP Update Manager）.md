---
kind: external_dependency
name: 自研外置更新服务（MVP Update Manager）
slug: mvp-update-manager
category: external_dependency
category_hints:
    - vendor_identity
    - sdk_real_api
    - client_constraint
scope:
    - '**'
source_files:
    - crates/mvp-updater/src/config.rs
    - crates/mvp-updater/src/net.rs
    - crates/mvp-updater/src/api.rs
    - crates/mvp-updater/src/version.rs
    - crates/mvp-updater/src/manifest.rs
    - crates/mvp-updater/src/apply.rs
    - crates/mvp-updater/Cargo.toml
    - scripts/package.ps1
---

### 身份与角色
- 项目自研的 Windows 桌面应用自动更新后端，部署在公网域名 `https://updateman.mvpclub.cc/`，HTTPS 可达。
- 通过产品标识 `MVP-Versatile-Player`（数字 ID 为 `7`）区分不同产品；客户端以 `platform=windows&architecture=x86_64&client_version=<当前版本>` 查询 latest 接口，服务端返回 `version`、`download_url`、`sha256`、`file_size`、`force_update`、`change_log` 等字段。
- 包形态为根平铺 zip 全量包，由 `scripts/package.ps1` 构建并输出 version/file_size/sha256 供后台登记。

### 集成方式
- 新增独立 crate `crates/mvp-updater`，分 check 侧（config/net/version/api/lib.rs）与 apply 侧（manifest/apply/main.rs），后者通过 `--features apply` 编译出 `mvp-updater.exe`。
- 主程序 `mvp-player` 在启动后约 2 秒按设置项 `check_updates_on_startup` 静默调用检查，发现新版本弹出 `Overlay::Update` 对话框；菜单「帮助 → 检查更新」可手动触发。
- 下载完成后由 `mvp-updater.exe` 独立进程执行：解压 zip 中央目录清单、比对 install.json 缓存、仅替换变化文件、原子替换旧安装目录并重启主程序；失败时回滚到原目录。

### 关键约束
- 安装目录必须可写；更新器不签名、不提权，无法覆盖受保护位置。
- 防降级：本地 semver 比较拒绝比已发布版本更旧的提示（服务端若错误地仍返回 `update_available: true`，客户端会兜底拦截）。
- 下载 URL 指向服务端登记的 `download_url`（通常即 Gitee release 附件直链），校验使用 SHA-256 + CRC32 两级校验。

### 已知问题（需服务端修复）
- 当 `client_version` 高于已发布版本时，服务端 `latest` 仍返回 `update_available: true`，应改为 false；客户端靠本地 semver 挡下，但属于服务端逻辑缺陷。
- 历史版本登记中曾出现 `download_url` 指向旧 tag、`file_size: 0`、`change_log` 为空的情况，发布时需确保三者一致。