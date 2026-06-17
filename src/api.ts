import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TunnelConfig, TunnelInfo, TunnelState } from "./types";

// ─── Queries ─────────────────────────────────────────────────────────────────

export const listTunnels = (): Promise<TunnelInfo[]> => invoke("list_tunnels");

export const parseCommand = (raw: string): Promise<TunnelConfig> =>
  invoke("parse_command", { raw });

// ─── CRUD ─────────────────────────────────────────────────────────────────────

export const addTunnel = (
  rawCommand: string,
  name?: string,
  group?: string | null,
  environment?: string | null,
): Promise<TunnelConfig> => invoke("add_tunnel", { rawCommand, name, group, environment });

export const updateTunnel = (config: TunnelConfig): Promise<void> =>
  invoke("update_tunnel", { config });

export const deleteTunnel = (id: string): Promise<void> =>
  invoke("delete_tunnel", { id });

// ─── Connection ───────────────────────────────────────────────────────────────

export const connectTunnel = (id: string): Promise<void> =>
  invoke("connect_tunnel", { id });

export const disconnectTunnel = (id: string): Promise<void> =>
  invoke("disconnect_tunnel", { id });

export const reconnectTunnel = (id: string): Promise<void> =>
  invoke("reconnect_tunnel", { id });

export const reconnectAll = (): Promise<void> => invoke("reconnect_all");

// ─── Password ─────────────────────────────────────────────────────────────────

export const submitPassword = (
  id: string,
  password: string,
  save: boolean
): Promise<void> => invoke("submit_password", { id, password, save });

// ─── Public key ───────────────────────────────────────────────────────────────

export const uploadPubkey = (
  id: string,
  pubkeyContent: string
): Promise<void> => invoke("upload_pubkey", { id, pubkeyContent });

export const deleteSavedPassword = (id: string): Promise<void> =>
  invoke("delete_saved_password", { id });

// ─── Events ───────────────────────────────────────────────────────────────────

export const onStateChanged = (
  cb: (payload: { id: string; state: TunnelState }) => void
): Promise<UnlistenFn> =>
  listen<{ id: string; state: TunnelState }>("tunnel://state-changed", (e) =>
    cb(e.payload)
  );

export const onPasswordRequired = (
  cb: (payload: { id: string; prompt: string }) => void
): Promise<UnlistenFn> =>
  listen<{ id: string; prompt: string }>("tunnel://password-required", (e) =>
    cb(e.payload)
  );
