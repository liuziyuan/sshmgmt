# sshmgmt — SSH 隧道管理器

一个跨平台桌面应用，用来管理 SSH 本地端口转发（`ssh -L` 隧道）。把那些又长又难记的 `ssh -L 3306:db.internal:3306 user@jump` 命令存成一条条隧道，点一下就连，状态一眼可见，不用再在终端里手敲和盯着窗口。

基于 Tauri 2 + React + Rust（[russh](https://github.com/Eugeny/russh)）实现，原生小体积，无需本地装 `ssh` 客户端。

## 背景 / 它解决什么

平时连内网数据库、Redis、内部服务，常常要先开一条 SSH 跳板隧道。痛点：

- 命令长、参数多，记不住也容易敲错；
- 终端窗口一关隧道就断，得一直留着；
- 同时开好几条，分不清哪条通哪条断；
- 断线后要手动重连。

sshmgmt 把这些隧道集中管理：

- **粘贴即用**：直接粘贴一条 `ssh -L ...` 命令，自动解析成隧道配置。
- **一键连接 / 断开 / 重连**，每条隧道一个状态灯（🟢 已连接 / 🟠 连接中 / ⚪ 未连接 / 🔴 失败）。
- **自动重连**：断线按退避策略自动重试。
- **存活探测**：启动时自动探测本机端口，识别已经建立的隧道——**无论是本应用建的，还是你在别的终端里手动 `ssh -L` 起的**，都会如实显示为已连接。
- **接管外部隧道**：你手动开的隧道一旦断掉，应用可自动接手重建，端口无缝衔接。
- **分组 / 环境标签**：按项目、按 dev/staging/prod 归类。
- **托盘常驻**：最小化到系统托盘后台保活，隧道不断。

> ⚠️ 当前版本**不支持**用「用户名 + 密码」方式登录 SSH 服务器。请先手动为跳板机配置好 SSH 公钥免密登录（把你的公钥加到服务器 `~/.ssh/authorized_keys`），再用本应用连接。

## 安装

### 下载安装包

到 [Releases](https://github.com/liuziyuan/sshmgmt/releases) 下载对应平台的包：

| 平台 | 下载 |
|------|------|
| macOS（Apple Silicon，M 系列）| `sshmgmt_x.y.z_aarch64.dmg` |
| macOS（Intel）| `sshmgmt_x.y.z_x64.dmg` |
| Windows | `sshmgmt_x.y.z_x64-setup.exe` / `.msi` |
| Linux | `.AppImage` 或 `.deb` |

不确定 Mac 芯片？终端执行 `uname -m`：`arm64` 选 aarch64，`x86_64` 选 Intel 包。

### ⚠️ macOS 提示「"sshmgmt" 已损坏，无法打开，你应该将它移到废纸篓」

**这不是包真的损坏**。因为本应用尚未做 Apple 代码签名 / 公证，macOS Gatekeeper 会给下载的文件打上「隔离」标记并拦截未签名应用，从而误报「已损坏」。

解决（把 app 拖进「应用程序」后，终端执行一次即可）：

```bash
xattr -dr com.apple.quarantine /Applications/sshmgmt.app
```

然后正常双击打开。之后不会再提示。

> 想彻底免除此提示，需要 Apple 开发者证书（$99/年）对应用签名并公证。在那之前，请用上面的命令绕过。

## 使用

1. **新增隧道**：点右上角「+ 新增隧道」，把一条完整的 ssh 命令粘进去，例如
   ```
   ssh -L 3306:db.internal:3306 -i ~/.ssh/id_ed25519 deploy@jump.example.com
   ```
   应用会解析出监听端口、目标、跳板机等信息。可再填项目名、环境标签。
2. **连接**：在列表对应行点「连接」，状态灯转绿即已通。
3. **断开 / 重连**：用对应按钮；右上角「🔄 全部重连」适合切换网络 / VPN 后批量重连。
4. **后台保活**：点最小化会隐藏到系统托盘，隧道保持连接；点托盘图标可重新唤出窗口；托盘右键菜单可「全部重连」或「退出」。
5. **已存在的隧道**：如果某个端口已被你在终端里手动开的隧道占用，应用会自动识别并显示为已连接，不会冲突。

### 退出行为说明

- **最小化 / 失焦** → 隐藏到托盘，**隧道继续运行**。
- **托盘「退出」/ 关闭窗口 / Cmd+Q** → 进程结束，**应用内建立的隧道一并关闭**（在别的终端手动开的隧道不受影响）。

## 从源码构建（开发者）

需要 [Node.js](https://nodejs.org/)、[Rust](https://www.rust-lang.org/tools/install) 和 [Tauri 环境依赖](https://tauri.app/start/prerequisites/)。

```bash
npm install          # 装前端依赖
npm run tauri dev    # 开发模式（热重载）
npm run tauri build  # 出当前平台的安装包，产物在 src-tauri/target/release/bundle/
```

发布：改 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 三处 `version`，提交后打 tag 推送：

```bash
git tag v1.0.1 && git push origin v1.0.1
```

GitHub Actions（`.github/workflows/release.yml`）会自动在 macOS / Windows / Linux 上构建并把安装包传到一个草稿 Release，确认后点 Publish 即可对外下载。

## 技术栈

- **前端**：React 19 + TypeScript + Vite
- **后端**：Rust + Tokio + [russh](https://github.com/Eugeny/russh)（纯 Rust SSH 实现，无需系统 ssh）
- **框架**：[Tauri 2](https://tauri.app/)
- 密码 / 密钥保存在系统钥匙串（Keychain）。

## License

见 [LICENSE](./LICENSE)。
