mod commands;
mod manager;
mod model;
mod parser;
mod probe;
mod store;
mod tunnel;

use commands::AppManager;
use manager::TunnelManager;
use store::load_tunnels;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};
use tokio::sync::Mutex as TokioMutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("sshmgmt=debug")),
        )
        .init();

    let tunnels = load_tunnels();
    let manager = TunnelManager::new(tunnels);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(TokioMutex::new(manager) as AppManager)
        .setup(|app| {
            setup_tray(app)?;
            intercept_minimize(app);

            // Auto-detect tunnels already up on this machine (app leftover or a
            // manual `ssh -L` in a terminal) and reflect them as connected.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Give the webview time to mount and attach its event listener so
                // the state-changed emits are not missed.
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let mgr = handle.state::<AppManager>();
                mgr.lock().await.detect_existing(&handle).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_tunnels,
            commands::parse_command,
            commands::add_tunnel,
            commands::update_tunnel,
            commands::delete_tunnel,
            commands::connect_tunnel,
            commands::disconnect_tunnel,
            commands::reconnect_tunnel,
            commands::reconnect_all,
            commands::submit_password,
            commands::upload_pubkey,
            commands::delete_saved_password,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ─── 系统托盘 ──────────────────────────────────────────────────────────────────

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let reconnect_item = MenuItem::with_id(app, "reconnect", "全部重连", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_item, &reconnect_item, &sep, &quit_item])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or(tauri::Error::InvalidWindowHandle)?;

    TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .tooltip("SSH 隧道管理器")
        // 左键单击：切换窗口显示/隐藏
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                toggle_window(app);
            }
        })
        // 右键菜单点击
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => toggle_window(app),
            "reconnect" => {
                let app_h = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_h.state::<AppManager>();
                    let inner = app_h.clone();
                    state.lock().await.reconnect_all(&inner);
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn toggle_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

// ─── 最小化 → 隐藏到托盘 ──────────────────────────────────────────────────────

fn intercept_minimize(app: &tauri::App) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let win_clone = win.clone();
    win.on_window_event(move |event| {
        // Focused(false) fires when the window loses focus, which includes
        // minimize. We check is_minimized() after a short delay to distinguish
        // minimize from a plain focus-switch.
        if let WindowEvent::Focused(false) = event {
            let w = win_clone.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if w.is_minimized().unwrap_or(false) {
                    let _ = w.hide();
                }
            });
        }
    });
}
