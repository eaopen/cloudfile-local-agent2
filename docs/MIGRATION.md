# 新仓库迁移与历史边界

## 权威来源

来源 `eaopen/cloudfile-local-agent` 的 `agent-rust/agent22/`，2026-10-15 检查时 SHA `64ce5fa96af6fd48a1cab038f978464db9367b2b`。执行者必须先重新获取远程 HEAD。提取 Rust 子树相关提交历史，保留 Cargo.lock 和构建、测试脚本；单独复制源仓库根的 LICENSE/NOTICE，并调整 `.github/workflows/agent-rust.yml` 为新仓库的 `.github/workflows/ci.yml`。

来源中其他 Go/Swift/.NET 文件均不进入新仓库。Go Agent2 未正式发布，不支持数据和密钥迁移。旧仓库 `agent-rust` 暂保留只读参考，不再继续提交 Rust；旧 Go `dev` 不因新项目创建而重置或删除。

## 仓库命名

- GitHub：`eaopen/cloudfile-local-agent2`
- 长期开发：`dev`；短期分支：`feature/<name>`；发行 `v2.2.x`
- Cargo：保持 `cloudfile-core`、`cloudfile-native-host`、`cloudfile-desktop`，避免无用重命名
- 正式产品：CloudFile Agent 2.2；开发产品：CloudFile Agent 2.2 Dev

## 机械迁移后必须扫描

`agent22/`、`agent-rust`、`cloudfile-local-agent/`、GitHub 旧 URL、`root.parents[1]`、旧 `.github` 路径、`working-directory`、`dist`、`LICENSE`、`NOTICE`。不要批量替换 CloudFile Server API 路径或 Native Host 协议字符串。

`scripts/prepare-extension.py` 要允许通过路径参数或环境变量提供 Chrome 扩展源码。不要把 Chrome 扩展直接 vendoring 到新仓库，也不要为迁移修改正式 Chrome 插件。

## Git 提交和保护

先基线检查，再 `git subtree split --prefix=agent22 origin/agent-rust`。禁止强推、禁止覆盖已有新仓库；如果缺乏组织建仓权限，只能制作本地可验证的整理结果并报告阻塞。新仓库通过 CI 后才可宣告独立构建完成；只有正式双平台业务验收以后才可讨论旧版停发或清理。

## 本次迁移记录（2026-10-15）

| 项 | 值 |
|---|---|
| 来源仓库 | `eaopen/cloudfile-local-agent` |
| 来源分支 | `agent-rust` |
| 迁移来源 SHA | `64ce5fa96af6fd48a1cab038f978464db9367b2b` |
| 提取方式 | `git subtree split --prefix=agent22 origin/agent-rust -b agent2-extracted` |
| 提取后命令 | `git rev-parse agent2-extracted` |
| 提取后首提交 SHA | `bc7a72b48904cda15717c26e08eca495a67403e3`（9 个提交，含原 Rust 开发历史） |
| 目标仓库 | `eaopen/cloudfile-local-agent2`，默认分支 `dev` |
| 未进入新仓库 | `cmd/`、`internal/`、`windows/`、`macos/`、`src/`、`test/`、`tests/`、`go.mod`、`agent22/` 以外的 Go/.NET/Swift 资产 |

`subtree split` 会重写提交 SHA，因此 `agent2-extracted` 的提交与来源 `agent-rust` 上的对应提交 SHA 不同、提交信息一致；来源仓库未被修改，`agent-rust` 分支保留为只读回退参考。
