[English](README.md) | [简体中文](README.zh-CN.md)

# portunnel — SSH Tunnel Manager

A cross-platform desktop app for managing SSH local port forwarding (`ssh -L` tunnels). Save those long, easy-to-forget `ssh -L 3306:db.internal:3306 user@jump` commands as tunnels: one click to connect, status at a glance — no more typing them by hand in a terminal and babysitting windows.

Built on Tauri 2 + React + Rust ([russh](https://github.com/Eugeny/russh)); small native footprint, no local `ssh` client required.

## Background / what it solves

Connecting to internal databases, Redis, or other services often requires an SSH jump-host tunnel first. Pain points:

- Long commands, many flags — hard to remember and easy to mistype;
- Closing the terminal window kills the tunnel;
- Running several at once, unclear which are up and which are down;
- Manual reconnect after a dropped connection.

portunnel manages these tunnels in one place:

- **Paste and go**: paste an `ssh -L ...` command and it is parsed into a tunnel config automatically.
- **One-click connect / disconnect / reconnect**, with a status light per tunnel (🟢 connected / 🟠 connecting / ⚪ disconnected / 🔴 failed). The disconnect button also takes effect immediately during the yellow "connecting" phase — no getting stuck.
- **Auto-reconnect**: dropped connections retry with backoff; an unreachable jump host is declared timed out after at most 15 seconds, so you never sit on an endless yellow light.
- **Password login supported**: connect even without pubkey auth on the jump host — just enter the username and password in the prompt (also compatible with servers that only offer `keyboard-interactive`, e.g. corporate AD/Kerberos environments). Passwords can be stored in the system keychain and auto-filled next time.
- **Automatic public-key upload after connect**: after a successful password login, check "Upload public key after connect" to append your local public key to the server's `authorized_keys` and go passwordless from then on.
- **Second-layer target host login check**: if a forward targets another machine's SSH port (e.g. `-L 10010:10.125.162.245:22`, usually so you can hop again through it), the app verifies that the target host is actually loggable at connect time — pubkey first (remembered username or derived from the jump username), then a password prompt on failure; the light only turns green once both hops verify.
- **Liveness probing**: on startup the app probes local ports and recognizes already-established tunnels — **whether created by this app or by a manual `ssh -L` in another terminal** — and honestly shows them as connected.
- **Taking over external tunnels**: if a tunnel you started manually goes down, the app can rebuild it automatically with a seamless port handoff.
- **Groups / environment tags**: organize by project and by dev/staging/prod.
- **Tray-resident**: minimize to the system tray to keep tunnels alive in the background.
- **Error notices you can't miss**: connection failures, auth failures, save failures and the like pop a persistent notice bar that only closes when you click ✕; success notices auto-dismiss after a few seconds. Multiple notices stack without overwriting each other.

> 🔑 On first connect, if the jump host / target host doesn't have pubkey auth set up yet, a dialog asks for the username and password; you can choose to remember the password, and/or upload your public key after a successful connect so you never type it again.

## Installation

### Download an installer

Grab the package for your platform from [Releases](https://github.com/liuziyuan/portunnel/releases):

| Platform | Download |
|------|------|
| macOS (Apple Silicon, M series) | `portunnel_x.y.z_aarch64.dmg` |
| macOS (Intel) | `portunnel_x.y.z_x64.dmg` |
| Windows | `portunnel_x.y.z_x64-setup.exe` / `.msi` |
| Linux | `.AppImage` or `.deb` |

### Upgrading from the old version

portunnel is a rename of sshmgmt: after upgrading, your existing tunnel configs, groups and keychain passwords carry over automatically — nothing to do by hand (the first launch migrates the data directory automatically; old data is kept, not deleted, so you can roll back to the old version anytime).

If you had the old version installed, it's recommended to uninstall it after installing portunnel: while both are present their configs stay separate and will diverge (keychain passwords are shared between the two).

Not sure which chip your Mac has? Run `uname -m` in a terminal: `arm64` → aarch64, `x86_64` → Intel package.

### ⚠️ macOS says "portunnel" is damaged and can't be opened; you should move it to the Trash

**The package is not actually damaged.** The app has no Apple code signing / notarization yet, so macOS Gatekeeper flags downloaded files with a "quarantine" attribute and blocks unsigned apps, producing this false "damaged" report.

Fix (after dragging the app into "Applications", run this once in a terminal):

```bash
xattr -dr com.apple.quarantine /Applications/portunnel.app
```

Then open it normally by double-clicking. You won't be prompted again.

> To eliminate the prompt entirely, the app needs to be signed and notarized with an Apple developer certificate ($99/year). Until then, use the command above to bypass it.

## Usage

1. **Add a tunnel**: click "+ Add tunnel" in the top right and paste a full ssh command, e.g.
   ```
   ssh -L 3306:db.internal:3306 -i ~/.ssh/id_ed25519 deploy@jump.example.com
   ```
   The app parses the listen port, target, jump host, and so on. You can also fill in a project name and environment tag.
2. **Connect**: click "Connect" on the tunnel's row; the status light turns green when it's up. If the jump host (or the second-layer target host) needs a password, a dialog asks for it; clicking "Cancel" gives up on this connect attempt and disconnects immediately, rather than leaving a zombie stuck on yellow.
3. **Disconnect / reconnect**: use the matching buttons; disconnecting during the yellow "connecting" phase also takes effect immediately; "🔄 Reconnect all" in the top right is handy for batch reconnects after switching networks / VPN.
4. **Keep alive in the background**: minimizing hides the window to the system tray and keeps tunnels connected; click the tray icon to bring the window back; the tray menu offers "Reconnect all" and "Quit".
5. **Pre-existing tunnels**: if a port is already occupied by a tunnel you started manually in a terminal, the app recognizes it and shows it as connected — no conflict.
6. **Error notices**: any failed operation (connect, save, delete, password auth, etc.) pops a red persistent notice bar at the top, closed manually with ✕, so you never miss it; success notices auto-dismiss after a few seconds.

### Exit behavior

- **Minimize / losing focus** → hides to the tray; **tunnels keep running**.
- **Tray "Quit" / closing the window / Cmd+Q** → the process exits and **tunnels created inside the app close with it** (tunnels you started manually in other terminals are unaffected).

## Building from source (developers)

Requires [Node.js](https://nodejs.org/), [Rust](https://www.rust-lang.org/tools/install) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/).

```bash
npm install          # install frontend dependencies
npm run tauri dev    # development mode (hot reload)
npm run tauri build  # build the installer for the current platform; output in src-tauri/target/release/bundle/
```

Releasing: bump `version` in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`, commit, then tag and push:

```bash
git tag v1.1.0 && git push origin v1.1.0
```

GitHub Actions (`.github/workflows/release.yml`) builds on macOS / Windows / Linux automatically and uploads the installers to a draft Release; confirm it and click Publish to go public.

## Tech stack

- **Frontend**: React 19 + TypeScript + Vite
- **Backend**: Rust + Tokio + [russh](https://github.com/Eugeny/russh) (pure-Rust SSH implementation, no system ssh)
- **Framework**: [Tauri 2](https://tauri.app/)
- Passwords / remembered target-host usernames are stored in the system keychain (Keychain / Credential Manager).

### How authentication works

On every connect, jump-host authentication runs in two phases, each on its own SSH connection: first only local private keys are tried (`~/.ssh/id_ed25519` etc.); if that fails, a brand-new connection is used for password / keyboard-interactive. That way, even on servers with a low `MaxAuthTries`, the pubkey retries can't exhaust the password attempt's budget. Forwards targeting `:22` work the same way: pubkey first (remembered username or derived from the jump username), then a password prompt as fallback. The whole connect+authenticate process can be interrupted by "Disconnect" at any time, so you never stay stuck on yellow.

## License

See [LICENSE](./LICENSE).
