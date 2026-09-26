---
kind: external_dependency
name: Gitee Release 作为安装包托管与下载页
slug: gitee-release
category: external_dependency
category_hints:
    - vendor_identity
    - client_constraint
scope:
    - '**'
source_files:
    - crates/mvp-player/src/ui/dialogs.rs
---

### 身份与角色
- 项目的代码仓库位于 `https://gitee.com/mvp-group1/mvp-versatile-player`，Release 页面 `releases/tag/vX.Y.Z` 作为用户手动下载的入口。
- 早期计划从 Gitee Release API (`/api/v5/repos/.../releases/latest`) 获取附件直链，后经确认改用自研 MVP Update Manager；但 Gitee 仍作为最终安装包托管源（`DOWNLOAD_PAGE` 常量指向 `https://gitee.com/mvp-group1/mvp-versatile-player/releases/tag/v1.0.2`）。
- Gitee Release 附件匿名可下载：浏览器访问 `releases/download/vX.Y.Z/<zip>` 会 302 重定向到 OSS 签名直链（`foruda.gitee.com`），无需登录或 Cookie。

### 使用方式
- 更新对话框中的「打开下载页面」按钮直接跳转至 Gitee Release tag 页，让用户自行选择对应版本的 zip 包。
- 版本号采用带 `v` 前缀的 tag（如 `v1.0.2`），与 Cargo workspace 版本号需保持同步。

### 注意事项
- 硬编码的 tag URL 会在发版后过期；更耐用的写法是跳转到 releases 列表页或按服务端返回的版本号拼接 tag。
- 发布流程：先通过 `scripts/package.ps1` 产出 zip，再上传到 Gitee Release 并在 MVP Update Manager 后台登记对应的 version/sha256。