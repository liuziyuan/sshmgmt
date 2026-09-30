import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { save, open } from "@tauri-apps/plugin-dialog";
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
  save: boolean,
  pubkeyPath: string | null,
  username?: string,
): Promise<void> =>
  invoke("submit_password", { id, password, save, pubkeyPath, username });

export interface PublicKeyInfo {
  path: string;
  name: string;
  content: string;
  has_private: boolean;
}

export const listPublicKeys = (): Promise<PublicKeyInfo[]> =>
  invoke("list_public_keys");

export const deleteSavedPassword = (id: string): Promise<void> =>
  invoke("delete_saved_password", { id });

// ─── Import / Export ─────────────────────────────────────────────────────────

export interface ImportSummary {
  imported: number;
  skipped: number;
  skipped_names: string[];
  warnings: string[];
}

export const exportTunnels = (path: string): Promise<number> =>
  invoke("export_tunnels", { path });

export const importTunnels = (path: string): Promise<ImportSummary> =>
  invoke("import_tunnels", { path });

/** Native save dialog for the export file; returns null when cancelled. */
export const pickExportPath = (): Promise<string | null> =>
  save({
    defaultPath: "tunnels.sshmgmt.json",
    filters: [{ name: "SSH 隧道配置", extensions: ["sshmgmt.json", "json"] }],
  });

/** Native open dialog for an import file; single selection, null when cancelled. */
export const pickImportPath = (): Promise<string | null> =>
  open({
    multiple: false,
    directory: false,
    filters: [{ name: "SSH 隧道配置", extensions: ["sshmgmt.json", "json"] }],
  }) as Promise<string | null>;

// ─── Group ordering ──────────────────────────────────────────────────────────

export const getGroupOrder = (): Promise<string[]> => invoke("get_group_order");

export const setGroupOrder = (order: string[]): Promise<void> =>
  invoke("set_group_order", { order });

export const getCollapsedGroups = (): Promise<string[]> =>
  invoke("get_collapsed_groups");

export const setCollapsedGroups = (groups: string[]): Promise<void> =>
  invoke("set_collapsed_groups", { groups });

// ─── Events ───────────────────────────────────────────────────────────────────

export const onStateChanged = (
  cb: (payload: { id: string; state: TunnelState }) => void
): Promise<UnlistenFn> =>
  listen<{ id: string; state: TunnelState }>("tunnel://state-changed", (e) =>
    cb(e.payload)
  );

export interface PasswordRequiredPayload {
  id: string;
  prompt: string;
  layer: "jump" | "target";
  host: string;
  needUsername: boolean;
}

export const onPasswordRequired = (
  cb: (payload: PasswordRequiredPayload) => void
): Promise<UnlistenFn> =>
  listen<PasswordRequiredPayload>("tunnel://password-required", (e) =>
    cb(e.payload)
  );

export interface NoticePayload {
  id: string;
  level: "success" | "warn" | "error";
  message: string;
}

export const onNotice = (
  cb: (payload: NoticePayload) => void
): Promise<UnlistenFn> =>
  listen<NoticePayload>("tunnel://notice", (e) => cb(e.payload));
