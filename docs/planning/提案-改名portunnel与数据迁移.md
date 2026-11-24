---
kind: proposal
status: accepted
summary: 项目改名 portunnel：可见层换新、底层身份保留、数据目录一次性迁移，版本 2.0.0
---

# 提案：改名 portunnel 与数据目录迁移

> 状态：已拍板，待实施
> 关联：`store.rs`、`transfer.rs`、`tauri.conf.json`、`Cargo.toml`、`package.json`、`api.ts`、`lib.rs`、`README.md`、`CLAUDE.md`、`.github/workflows/release.yml`
> 读者：后续实施者（预期经 /pipeline 执行）

## 0. 一句话结论

项目更名为 portunnel：对外可见名（GitHub repo、安装包名、文档、CI 文案）换新；底层身份（应用唯一标识、钥匙串 service、导出格式标识）保留以兼容 sshmgmt 时代；数据目录迁至 `portunnel/` 并在启动时自动从旧目录迁移（复制、保留旧目录）；版本直接发布 2.0.0。

## 1. 问题

- 现名 `sshmgmt` 作为品牌辨识度差、不含"隧道/转发"语义（候选名经 GitHub API 实测：`portunnel` 同名仓库仅 3 个且全为 0 星，撞名风险≈0）。
- 项目已有真实用户与历史发布（v1.2.0），改名必须**平稳过渡**：老用户升级后隧道配置、分组、钥匙串密码全部无缝延续，不允许"配置消失"式断层。

## 2. 现状核对

| 项 | 当前值 | 位置 |
|---|---|---|
| productName | `sshmgmt` | `tauri.conf.json:3` |
| 应用唯一标识（identifier） | `com.sshmgmt.app` | `tauri.conf.json:5` |
| 数据目录 | `dirs::config_dir()/sshmgmt/`（macOS 实际为 `~/Library/Application Support/sshmgmt/`，Linux 为 `~/.config/sshmgmt/`；CLAUDE.md 现描述只对 Linux 成立，需顺带修正） | `store.rs:8-12` |
| 数据文件 | `tunnels.json`、`group_order.json`、`collapsed_groups.json` 共三个 | `store.rs:14-24` |
| 钥匙串 service | `"sshmgmt"`（账号约定 `user@host:port` 与 `__targetuser__@host:port`） | `store.rs:6` |
| 导出格式标识 / 版本 | `"sshmgmt-tunnels"` / v1，导入时严格校验 format 与 version | `transfer.rs:15-16`、`read_import` |
| 导出默认文件名 / 过滤器扩展名 | `tunnels.sshmgmt.json` / `sshmgmt.json` | `api.ts:83-92` |
| crate 名 / lib 名 | `sshmgmt` / `sshmgmt_lib`（`main.rs` 引用 `sshmgmt_lib::run()`） | `Cargo.toml:2,9`、`main.rs:5` |
| 日志过滤器 | `"sshmgmt=debug"`（跟随 crate 名，需同步） | `lib.rs:26` |
| README | 标题、安装包文件名表格、macOS"已损坏"提示（含 `/Applications/sshmgmt.app` 与 xattr 命令）、Releases 链接 | `README.md` |
| CLAUDE.md | 名称、Releases 链接、数据路径描述 | `CLAUDE.md` |
| CI 文案 | `releaseName: 'sshmgmt ${{ github.ref_name }}'`、releaseBody 中 app 路径 | `.github/workflows/release.yml:67-68` |
| git remote | `git@github.com:liuziyuan/sshmgmt.git` | 本地 git 配置 |

## 3. 方案

### 3.1 名称变更矩阵

| 层 | 处理 | 理由 |
|---|---|---|
| GitHub repo 名 | 改 `portunnel` | 品牌层，无兼容问题；旧 URL 由 GitHub 自动重定向 |
| productName | 改 `portunnel` | 可见层；窗口标题与 README 标题维持"SSH 隧道管理器" |
| 数据目录 | 迁移式换新为 `portunnel/` | 老用户配置自动延续（规格见 3.2） |
| identifier | **保留 `com.sshmgmt.app`** | 本项目无通知/文件关联等系统权限依赖，换新无实际收益；保留贴合"兼容底层名称"诉求 |
| 钥匙串 service | **保留 `"sshmgmt"`** | 密码零迁移、零风险延续；仅 macOS"钥匙串访问"可见，属深藏底层名 |
| 导出格式标识 | **保留 `"sshmgmt-tunnels"` v1** | 文件格式命名空间，用户不可见；保留换来新旧版本导出文件**双向互通**（换新+兼容读只能单向，旧版认不出新版文件） |
| crate / npm 包名 | 改 `portunnel` | 纯内部标识，机械同步 |
| 文档与 CI 文案 | 改 portunnel | 纯文档 |

### 3.2 数据目录迁移规格（路线 B：一次性迁移）

- **触发**：应用启动时、任何读配置动作之前，一次性判定。
- **判定逻辑**：
  1. 新目录 `portunnel/` 存在 → 直接使用新目录（迁移已完成或新装用户）。
  2. 新目录不存在 且 旧目录 `sshmgmt/` 存在 → 将旧目录下的三个文件**复制**到新目录，之后使用新目录。
  3. 都不存在 → 使用新目录（从零开始）。
- **复制而非移动**：旧目录保留原样，回滚装旧版本时数据完好。
- **失败降级**：复制 IO 失败 → 按新目录空配置启动，不崩溃；需通过现有 banner 系统（`notify("warn", ...)`）告知用户"配置迁移失败，可重装重试"，旧数据不受影响。
- **已知代价**：新旧版本并存期间各写各的目录会数据分叉——通过发布文案提醒"安装新版后卸载旧版"缓解。
- **钥匙串无需迁移**：service 未变，密码自动延续。

### 3.3 版本与发布

- 版本号四处同步为 **2.0.0**：`npm version 2.0.0 --no-git-tag-version`（package.json + package-lock.json）、`Cargo.toml`、`tauri.conf.json`，改后 `cargo build` 同步 Cargo.lock。
- GitHub repo 改名（用户在 GitHub 网页执行，或授权后用 `gh` 命令执行）→ 更新本地 remote。
- 发布流程照 CLAUDE.md：PR → merge → `git tag v2.0.0` 推送触发 CI → 人工 Publish。**tag 是真正触发发布的唯一步骤，不能省。**
- releaseBody 更新要点：app 路径改为 `/Applications/portunnel.app`；说明"由 sshmgmt 更名而来，隧道配置与密码自动延续"；提醒安装后卸载旧版 sshmgmt.app（新旧并存期间配置各自独立）。

## 4. 边界与不变式（验收标准）

1. 老用户（存在旧数据目录）升级 2.0.0：首次启动后隧道列表、分组顺序、折叠状态、已存密码全部可用，零手动操作。
2. 迁移后旧目录仍存在且内容不变；回滚装 1.2.0 一切照旧。
3. 新装用户（无旧目录）：直接使用 `portunnel/`，行为与旧版一致。
4. 导出文件双向互通：1.2.0 导出的文件可导入 2.0.0；2.0.0 导出的文件可导入 1.2.0。
5. identifier、钥匙串 service、导出格式标识三个字符串在代码中不变。
6. 迁移失败不崩溃、不丢旧数据，且用户可感知（banner）。
7. `npm run build`、`cargo build`、`cargo test` 通过；`tunnel.rs` 保持零警告。

## 5. 决策项与取舍（均已拍板）

1. **数据目录迁移式换新（路线 B）**——确认。否决"优先读旧目录"（存在性判定永久分叉、误伤残留、清理旧目录即数据跳变）。
2. **底层保留清单：钥匙串 service、identifier、导出格式标识全保留**——确认。各项换新均无超过保留的收益（换钥匙串 service 引入密码搬运与丢失风险；换 identifier 系统仍视为新应用且 .app 残留在两种选择下相同；换导出格式使双向兼容降为单向）。
3. **版本 2.0.0**——人类拍板。否决提案推荐的 1.3.0，理由：大更新需要版本信号传达"这版不一样"。
4. **repo 名 `portunnel`**——确认（撞名已实测排除）。
5. **显示名划分**——窗口标题、README 标题维持"SSH 隧道管理器"；安装包名/应用名用 `portunnel`（中文 productName 在 Windows 安装路径与命令行场景有编码与输入不便）。

无待定项。

## 6. 风险与对策

| 风险 | 对策 |
|---|---|
| Windows 安装器在 productName 变化后能否原位覆盖升级旧版（升级标识派生自 productName，行为待实测） | 实施期实测：先装 1.2.0 再装 2.0.0；若并存属预期行为，发布文案说明手动卸载旧版 |
| 迁移失败 | 降级空配置启动 + banner 提醒，旧数据原地不损，重装即重试 |
| repo 改名后旧链接依赖重定向 | 同步更新 README / CLAUDE.md / remote；不在旧名下新建仓库 |
| 新旧版本并存数据分叉 | 发布文案明确提醒卸载旧版 |
| Cargo.lock 与 crate 名同步遗漏 | 改 `Cargo.toml` 后跑 `cargo build` 自动同步 |

## 7. 不做什么（非目标）

- 不迁移钥匙串条目（service 不变，密码自动延续）。
- 不改导出格式标识与版本（保持 `"sshmgmt-tunnels"` v1）。
- 不删除旧数据目录。
- 不改隧道连接状态机（`tunnel.rs`）、认证流程、转发逻辑。
- 不在本提案内处理"新增隧道表单模式""转发类型完整支持"两份既有提案的功能内容（若合并实施，版本号共用 2.0.0，但改名与功能分属不同工作项）。

## 8. 实现清单

1. `tauri.conf.json`：`productName` → `portunnel`；`version` → `2.0.0`；identifier 不动。
2. `Cargo.toml`：`name` → `portunnel`、lib `name` → `portunnel_lib`；同步 `main.rs` 引用；跑 `cargo build` 同步 Cargo.lock。
3. `package.json` / `package-lock.json`：`npm version 2.0.0 --no-git-tag-version` + `name` → `portunnel`。
4. `store.rs`：`config_dir()` 返回 `portunnel/`；新增一次性迁移函数（3.2 规格：复制三文件、旧目录保留、失败降级）；迁移失败接入 banner 通知；在启动最早点（`lib.rs` 初始化路径、首次读配置前）调用。
5. `lib.rs`：日志 EnvFilter `"portunnel=debug"`（跟随新 crate 名）。
6. `api.ts`：导出默认文件名 → `tunnels.portunnel.json`、过滤器扩展 → `portunnel.json`。
7. `transfer.rs`：测试临时目录名 → `portunnel-transfer-tests`；`EXPORT_FORMAT` 不动。
8. `README.md`：标题、安装包文件名表格、macOS"已损坏"提示路径与 xattr 命令、Releases 链接。
9. `CLAUDE.md`：名称、Releases 链接、数据路径描述修正（注明 macOS 实际位置）、钥匙串约定段落核对（service 名不变）。
10. `.github/workflows/release.yml`：`releaseName` → `portunnel ${{ github.ref_name }}`；releaseBody 路径与文案按 3.3 更新。
11. GitHub repo 改名 + 本地 remote 更新（`git@github.com:liuziyuan/portunnel.git`）。
12. 验证：`npm run build`、`cargo build`、`cargo test`；手动验证迁移三态（旧目录存在→复制且旧目录保留；新目录已存在→跳过；都不存在→空开始）。
13. 发布：PR → merge → `git tag v2.0.0 && git push origin v2.0.0` → CI 构建完成后人工 Publish。
