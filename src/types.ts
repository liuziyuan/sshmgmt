/**
 * Rule kind. Known kinds are "local" | "dynamic" | "remote"; the backend may
 * also return a kind written by a newer app version, which this version
 * preserves but cannot run — hence the open string tail.
 */
export type ForwardKind = "local" | "dynamic" | "remote" | (string & {});

export interface ForwardSpec {
  local_port: number;
  remote_host: string;
  remote_port: number;
  /** Rule kind; the backend always serializes it (older JSON defaults to local). */
  kind: ForwardKind;
  /** Remote-only server-side bind address; absent means "localhost". */
  remote_bind?: string;
}

export interface Hop {
  user: string;
  host: string;
  port: number;
}

export interface TunnelConfig {
  id: string;
  name: string;
  raw_command: string;
  jump_user: string;
  jump_host: string;
  jump_port: number;
  /** Authoritative jump chain; absent on configs written by older versions. */
  hops?: Hop[];
  forwards: ForwardSpec[];
  bind_all: boolean;
  identity_file: string | null;
  auto_reconnect: boolean;
  group?: string | null;
  environment?: string | null;
}

/**
 * Jump chain for display: the authoritative `hops`, else a single hop
 * synthesized from the legacy mirror fields (v1.4 and earlier configs).
 */
export function resolveHops(config: TunnelConfig): Hop[] {
  if (config.hops && config.hops.length > 0) return config.hops;
  if (config.jump_host) {
    return [{ user: config.jump_user, host: config.jump_host, port: config.jump_port }];
  }
  return [];
}

/**
 * A structured backend message: stable code plus interpolation params.
 * Field-identical with the Rust `Msg` enum in src-tauri/src/model.rs (C1).
 * Narrowed to the full discriminated union of codes in S6; the loose string
 * form keeps the S1 i18n skeleton compiling standalone.
 */
export interface LocalizedMessage {
  code: string;
  params?: Record<string, unknown>;
}

export type TunnelState =
  | { type: "Disconnected" }
  | { type: "Connecting" }
  | { type: "Connected" }
  | { type: "Reconnecting" }
  | { type: "Failed"; message: string }
  | { type: "PasswordRequired" };

export interface TunnelInfo {
  config: TunnelConfig;
  state: TunnelState;
}
