# CLAUDE.md

面向在本仓库（sshmgmt）工作的 AI 编码助手的项目说明。

## 项目是什么

跨平台桌面应用，管理 SSH 本地端口转发（`ssh -L` 隧道）。基于 **Tauri 2 + React 19 + TypeScript** 前端、**Rust + Tokio + [russh](https://github.com/Eugeny/russh)**（纯 Rust SSH 实现）后端。不依赖系统 `ssh` 客户端，所有连接、认证、转发都是自己用 `russh` 实现的。

## 目录结构

```
src/                        前端（React + TS）
  App.tsx                   顶层状态、事件监听、错误/成功气泡（banner）系统
  api.ts                    对 Tauri invoke/listen 的类型化封装，前后端事件/命令的唯一入口
  types.ts                  与 Rust model.rs 手动保持同步的 TS 类型
  components/
    TunnelList.tsx           隧道列表、状态灯、连接/断开/重连/删除按钮
    TunnelEditor.tsx          新增/编辑隧道弹窗（粘贴 ssh 命令解析预览）；导出 Overlay 供其他弹窗复用
    PasswordModal.tsx         密码/用户名输入弹窗，公钥选择与上传选项
    UploadKeyModal.tsx        已废弃（手动上传公钥流程），仅保留占位注释

src-tauri/src/               后端（Rust）
  lib.rs                     Tauri 入口：托盘、菜单、最小化拦截、command 注册
  commands.rs                 #[tauri::command] 入口，前端 invoke 的对应实现，薄封装、委托给 manager/tunnel
  manager.rs                  TunnelManager：隧道生命周期（start/stop/reconnect）、运行时 handle 表、状态表
  tunnel.rs                   核心：连接、两阶段认证、端口转发、断线重连的状态机（见下方"认证与连接状态机"）
  parser.rs                   把一条 `ssh ...` 命令行解析成 TunnelConfig（不支持 -J，只支持单跳 -L）
  model.rs                    Rust 侧数据结构（TunnelConfig/TunnelState/...），需与 src/types.ts 手动保持一致
  store.rs                    本地持久化（~/.config/sshmgmt/tunnels.json）+ 系统钥匙串读写（密码、记住的目标主机用户名）
  probe.rs                    探测本机端口是否已被（本应用或外部）健康的隧道占用
```

## 常用命令

```bash
npm install            # 装前端依赖
npm run tauri dev      # 开发模式（前端热重载 + Rust 后端）
npm run build           # 仅前端：tsc 类型检查 + vite build
npm run tauri build     # 出当前平台安装包，产物在 src-tauri/target/release/bundle/

cd src-tauri
cargo build             # 仅编译 Rust 后端
cargo test               # 跑 parser.rs 的解析单测
cargo clippy --lib       # lint（manager.rs/parser.rs 有几条历史遗留的风格警告，属已知项，不必顺手修）
```

改完前端 **务必** 跑 `npx tsc --noEmit`（或 `npm run build`）；改完 Rust **务必** 跑 `cargo build`（关注新增警告，`tunnel.rs` 应保持零警告）。

## 认证与连接状态机（`tunnel.rs`，改这部分前务必通读）

每条隧道由 `run_tunnel` 驱动一个 `Connecting → (Connected | Failed | Reconnecting)` 循环，真正的连接工作在 `connect_and_forward` → `establish_session` 里：

1. **跳板机认证分两阶段、各用独立的 SSH 连接**：阶段一只试本地私钥（`~/.ssh/id_ed25519` 等，或 `-i` 指定的），阶段二换一条全新连接再走密码 / `keyboard-interactive`。**不要合并成一条连接**——某些服务器（尤其是 AD/Kerberos 环境）`MaxAuthTries` 很低，公钥的多签名哈希重试（见 `try_publickey_auth`）会把认证次数在密码认证轮到之前就耗尽，导致服务器直接断连、被误判为"密码错误"。这是本项目修过的一个真实 bug，回归请参考 `try_password_or_kbi` / `connect_jump_host` 的注释。
2. **目标为 `:22` 的转发会做二级登录校验**（`setup_second_layer`）：转发通常意味着"转发过去后还要再 ssh 一次"，所以连接时就顺带验证目标机能不能登录，而不是等用户手动连接失败才发现。同样是"先公钥（记住的用户名或从跳板机用户名推导）、失败再密码提示"的两阶段模式，原因同上。
3. **建连+认证整个过程可被 `Stop`/`Reconnect` 随时打断**：`establish_session` 的 future 与 `control_rx.recv()` 用 `tokio::select!` 竞速。这是因为密码提示最长等 5 分钟、TCP 连接也可能长时间挂起，如果不竞速，用户在"连接中"黄灯阶段点断开会没有任何效果（真实修过的 bug）。**改这块逻辑时不要把生成端口监听任务（`tokio::spawn` 那些）纳入竞速区间**，否则取消时会造成任务泄漏。
4. `connect_jump_host` 对 TCP+SSH 握手有 15 秒超时（`JUMP_CONNECT_TIMEOUT`），避免跳板机不可达时永远卡在黄灯。
5. 认证失败（`ConnectError::Fatal`）= 红灯、不重连；网络类错误（`ConnectError::Retriable`）= 走退避重连。新增错误路径时注意选对这两者，选错会导致"密码错误"却无限重试，或者"网络抖动"却直接判死。
6. `PasswordModal` 的"取消"按钮语义是**放弃本次连接并断开**（调用 `disconnectTunnel`），不是单纯关闭弹窗——否则后端还在原地等密码（最长 5 分钟），隧道会卡成一个看起来没反应的黄灯僵尸。新增任何"取消/关闭"密码类弹窗的地方，都要接入断开而不是只隐藏 UI。

## 前端反馈约定（`App.tsx` 的 banner 系统）

- 所有用户可感知的失败必须通过 `notify("error"/"warn", msg)` 弹出**顶部常驻提示条**，只能点 ✕ 关闭，不会自动消失。`success` 级几秒后自动消失。多条堆叠，不互相覆盖。
- `App.tsx` 里还监听了隧道状态从非 Failed → Failed 的跳变，自动弹一条错误 banner（隧道名 + 原因），这是为了覆盖"密码输错/认证失败"这类后端异步才知道结果的场景——新增任何会导致隧道 `Failed` 的后端错误路径，不需要额外前端改动就会自动弹出。
- 新增任何可能失败的操作（IPC 调用、表单校验之外的东西）时，记得接入 `notify`，不要只是 `console.error` 或静默失败。

## 发布 / 版本号

版本号存在 **4 个地方**，升级时必须同时改（否则安装包版本号和 tag 对不上）：

1. `package.json`（可用 `npm version <major|minor|patch> --no-git-tag-version` 一次性改好 package.json + package-lock.json）
2. `package-lock.json`（如果没用上面的命令，需要手动同步两处 `"version"`）
3. `src-tauri/Cargo.toml` 的 `version = "..."`
4. `src-tauri/tauri.conf.json` 的 `"version": "..."`

改完 `Cargo.toml` 后跑一次 `cargo build`，让 `Cargo.lock` 里 `name = "sshmgmt"` 对应的 `version` 自动同步。

发布流程：提交 → `git tag vX.Y.Z && git push origin vX.Y.Z` → GitHub Actions（`.github/workflows/release.yml`）在 macOS/Windows/Linux 上构建，产物进草稿 Release，人工确认后 Publish。

## 其他约定

- **UI 文案一律中文**（用户群体是中文使用者），代码注释可用英文（仓库历史注释以英文为主，新增代码请保持这个风格：注释英文、UI 字符串中文）。
- `src/types.ts` 的 `TunnelState`/`TunnelConfig` 等类型需要和 `src-tauri/src/model.rs` 手动保持字段一致，改一边记得改另一边。
- `store.rs` 里钥匙串账号命名有约定：密码用 `user@host:port`，记住的目标主机用户名用保留前缀 `__targetuser__@host:port` 区分，避免和真实用户名冲突。新增钥匙串条目类型时沿用"保留前缀"这个模式。
- `parser.rs` 目前只支持单跳 `-L`，明确不支持 `-J`（ProxyJump）；如果要支持多跳，需要同时评估 `tunnel.rs` 里 `direct-tcpip` 转发路径的改动量。
