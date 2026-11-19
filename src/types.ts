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

export interface TunnelConfig {
  id: string;
  name: string;
  raw_command: string;
  jump_user: string;
  jump_host: string;
  jump_port: number;
  forwards: ForwardSpec[];
  bind_all: boolean;
  identity_file: string | null;
  auto_reconnect: boolean;
  group?: string | null;
  environment?: string | null;
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
