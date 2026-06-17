use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use tauri::Manager;
use tokio::sync::mpsc;

use crate::model::{ForwardSpec, PasswordResponse, TunnelConfig, TunnelInfo, TunnelState};
use crate::probe::{self, PortStatus};
use crate::tunnel::{self, PasswordSenders, StateMap, TunnelControl};

struct TunnelHandle {
    control_tx: mpsc::Sender<TunnelControl>,
}

pub struct TunnelManager {
    /// Persistent configs (source of truth for storage)
    configs: Vec<TunnelConfig>,
    /// Runtime handles for running tunnels
    handles: HashMap<String, TunnelHandle>,
    /// Live state per tunnel (updated from tunnel tasks)
    pub state_map: StateMap,
    /// Pending password responses (tunnel tasks wait on these)
    pub password_senders: PasswordSenders,
}

impl TunnelManager {
    pub fn new(configs: Vec<TunnelConfig>) -> Self {
        Self {
            configs,
            handles: HashMap::new(),
            state_map: Arc::new(StdRwLock::new(HashMap::new())),
            password_senders: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub fn list_tunnels(&self) -> Vec<TunnelInfo> {
        let states = self.state_map.read().unwrap();
        self.configs
            .iter()
            .map(|c| TunnelInfo {
                state: states
                    .get(&c.id)
                    .cloned()
                    .unwrap_or(TunnelState::Disconnected),
                config: c.clone(),
            })
            .collect()
    }

    pub fn get_config(&self, id: &str) -> Option<&TunnelConfig> {
        self.configs.iter().find(|c| c.id == id)
    }

    /// Current live state of a tunnel (defaults to Disconnected if unknown).
    pub fn current_state(&self, id: &str) -> TunnelState {
        self.state_map
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or(TunnelState::Disconnected)
    }

    /// Re-emit the current state so the UI refreshes (idempotent green refresh).
    pub fn reemit_state(&self, id: &str, app: &tauri::AppHandle) {
        let state = self.current_state(id);
        tunnel::emit_state(app, id, &state);
    }

    /// Mark a tunnel as served by an external (e.g. terminal `ssh -L`) connection:
    /// show Connected and spawn a lightweight monitor that re-probes the forwards
    /// and flips to Disconnected once the external tunnel disappears.
    pub fn mark_external(&mut self, id: &str, app: &tauri::AppHandle) {
        let Some(config) = self.configs.iter().find(|c| c.id == id).cloned() else {
            return;
        };

        tunnel::update_state(&self.state_map, id, TunnelState::Connected);
        tunnel::emit_state(app, id, &TunnelState::Connected);

        let (control_tx, mut control_rx) = mpsc::channel(8);
        let state_map = self.state_map.clone();
        let app = app.clone();
        let id_owned = id.to_string();
        let forwards: Vec<ForwardSpec> = config.forwards.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = control_rx.recv() => {
                        // Stop / channel dropped: cancel monitoring, leave the
                        // external tunnel untouched.
                        if matches!(msg, Some(TunnelControl::Stop) | None) {
                            tunnel::update_state(&state_map, &id_owned, TunnelState::Disconnected);
                            tunnel::emit_state(&app, &id_owned, &TunnelState::Disconnected);
                            break;
                        }
                        // Reconnect on an external tunnel is a no-op; keep watching.
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        if probe::probe_forwards(&forwards).await != PortStatus::Working {
                            // External tunnel gone — take over with an app-managed
                            // tunnel (the port is free now, so bind won't clash).
                            let mgr = app.state::<crate::commands::AppManager>();
                            let mut m = mgr.lock().await;
                            m.handles.remove(&id_owned); // drop this monitor's handle
                            m.start_tunnel(&id_owned, &app);
                            break;
                        }
                    }
                }
            }
        });

        self.handles
            .insert(id.to_string(), TunnelHandle { control_tx });
    }

    pub fn add_config(&mut self, config: TunnelConfig) {
        self.configs.push(config);
    }

    pub fn update_config(&mut self, config: TunnelConfig) -> bool {
        if let Some(existing) = self.configs.iter_mut().find(|c| c.id == config.id) {
            *existing = config;
            true
        } else {
            false
        }
    }

    pub fn remove_config(&mut self, id: &str) -> bool {
        let before = self.configs.len();
        self.configs.retain(|c| c.id != id);
        self.configs.len() < before
    }

    pub fn configs(&self) -> &[TunnelConfig] {
        &self.configs
    }

    /// Start a tunnel. If already running, does nothing.
    pub fn start_tunnel(&mut self, id: &str, app: &tauri::AppHandle) -> bool {
        if self.is_running(id) {
            return false; // already running
        }
        // Drop a stale handle whose task has already exited (Failed / monitor ended).
        self.handles.remove(id);
        let Some(config) = self.configs.iter().find(|c| c.id == id).cloned() else {
            return false;
        };

        let (control_tx, control_rx) = mpsc::channel(8);
        let state_map = self.state_map.clone();
        let password_senders = self.password_senders.clone();
        let app = app.clone();

        tokio::spawn(tunnel::run_tunnel(
            config,
            control_rx,
            state_map,
            password_senders,
            app,
        ));

        self.handles.insert(id.to_string(), TunnelHandle { control_tx });
        true
    }

    /// On startup, probe every configured tunnel's local ports and mark the ones
    /// already serving (app leftover or an external terminal `ssh -L`) as green +
    /// monitored. Tunnels the app already manages are skipped.
    pub async fn detect_existing(&mut self, app: &tauri::AppHandle) {
        let ids: Vec<(String, Vec<ForwardSpec>)> = self
            .configs
            .iter()
            .map(|c| (c.id.clone(), c.forwards.clone()))
            .collect();
        for (id, forwards) in ids {
            if self.is_running(&id) {
                continue;
            }
            if probe::probe_forwards(&forwards).await == PortStatus::Working {
                self.mark_external(&id, app);
            }
        }
    }

    /// Stop a tunnel. Returns false if not running.
    pub fn stop_tunnel(&mut self, id: &str) -> bool {
        if let Some(handle) = self.handles.remove(id) {
            let _ = handle.control_tx.try_send(TunnelControl::Stop);
            true
        } else {
            false
        }
    }

    /// Stop then immediately restart a tunnel.
    pub fn reconnect_tunnel(&mut self, id: &str, app: &tauri::AppHandle) -> bool {
        // Send Reconnect signal if running; otherwise start fresh
        if self.is_running(id) {
            if let Some(handle) = self.handles.get(id) {
                let _ = handle.control_tx.try_send(TunnelControl::Reconnect);
            }
            true
        } else {
            self.start_tunnel(id, app)
        }
    }

    /// Stop all running tunnels, then restart them. For VPN switch scenarios.
    pub fn reconnect_all(&mut self, app: &tauri::AppHandle) {
        let ids: Vec<String> = self.handles.keys().cloned().collect();
        for id in &ids {
            self.stop_tunnel(id);
        }
        // Brief delay is handled by the tunnel tasks' reconnect backoff
        for id in &ids {
            self.start_tunnel(id, app);
        }
    }

    /// Deliver a password response to a waiting tunnel task.
    pub fn submit_password(&self, id: &str, password: String, save: bool) -> bool {
        let mut senders = self.password_senders.lock().unwrap();
        if let Some(tx) = senders.remove(id) {
            let _ = tx.send(PasswordResponse { password, save });
            true
        } else {
            false
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.handles
            .get(id)
            .map_or(false, |h| !h.control_tx.is_closed())
    }
}
