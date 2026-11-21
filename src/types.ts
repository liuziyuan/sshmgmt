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
 * Structured backend message, field-identical with the Rust `Msg` enum in
 * src-tauri/src/model.rs (C1). Adjacently tagged: `{ code, params? }`.
 * The discriminated union locks the code spelling at compile time.
 */
export type Msg =
  | { code: "noSupportedForwards" }
  | { code: "missingJumpConfig" }
  | { code: "reconnectRequested" }
  | { code: "sshSessionClosed" }
  | { code: "sshChannelError" }
  | { code: "bindFailed"; params: { addr: string; detail: string } }
  | { code: "remoteForwardTimeout"; params: { port: number; timeoutSecs: number } }
  | { code: "remotePortDenied"; params: { port: number } }
  | { code: "remoteForwardFailed"; params: { port: number; detail: string } }
  | { code: "remotePortOutOfRange"; params: { port: number } }
  | { code: "remotePortAllocated"; params: { addr: string; port: number } }
  | {
      code: "jumpConnectTimeout";
      params: { host: string; port: number; timeoutSecs: number };
    }
  | { code: "jumpConnectFailed"; params: { detail: string } }
  | {
      code: "hopChannelOpenFailed";
      params: { host: string; port: number; detail: string };
    }
  | {
      code: "hopConnectFailed";
      params: { host: string; port: number; detail: string };
    }
  | {
      code: "hopConnectTimeout";
      params: { host: string; port: number; timeoutSecs: number };
    }
  | {
      code: "targetChannelOpenFailed";
      params: { host: string; port: number; detail: string };
    }
  | { code: "targetConnectFailed"; params: { detail: string } }
  | { code: "jumpKeyAuthError"; params: { detail: string } }
  | { code: "jumpPasswordPhaseError"; params: { detail: string } }
  | { code: "targetKeyAuthError"; params: { detail: string } }
  | { code: "targetPasswordPhaseError"; params: { detail: string } }
  | {
      code: "hopAuthFailed";
      params: { user: string; host: string; hopIndex?: number; hopTotal?: number };
    }
  | { code: "targetAuthFailed"; params: { host: string } }
  | {
      code: "pubkeyUploaded";
      params: { host: string; hopIndex?: number; hopTotal?: number };
    }
  | {
      code: "pubkeyUploadFailed";
      params: { host: string; hopIndex?: number; hopTotal?: number };
    }
  | { code: "targetPubkeyUploaded"; params: { host: string } }
  | { code: "targetPubkeyUploadFailed"; params: { host: string } }
  | { code: "promptJumpPassword"; params: { user: string; host: string } }
  | { code: "promptTargetLogin"; params: { host: string } }
  | { code: "parseTokenizeFailed"; params: { detail: string } }
  | { code: "parseFlagNeedsArg"; params: { flag: string } }
  | { code: "parseInvalidPort"; params: { value: string } }
  | { code: "parseNoDestination" }
  | { code: "parseNoForwards" }
  | { code: "parseInvalidLocalPort"; params: { value: string } }
  | { code: "parseInvalidRemotePort"; params: { value: string } }
  | { code: "parseInvalidLSpec"; params: { spec: string } }
  | { code: "parseInvalidDSpec"; params: { spec: string } }
  | { code: "parseInvalidRSpec"; params: { spec: string } }
  | { code: "parseInvalidRListenPort"; params: { value: string } }
  | { code: "parseREmptyTarget"; params: { spec: string } }
  | { code: "parseInvalidRTargetPort"; params: { value: string } }
  | { code: "parseJEmptyHop"; params: { spec: string } }
  | { code: "parseJEmptyHost"; params: { hop: string } }
  | { code: "tunnelNotFound" }
  | { code: "noPendingPassword" }
  | { code: "importKeychainFailed"; params: { name: string; detail: string } }
  | {
      code: "importHopKeychainFailed";
      params: { name: string; hopIndex: number; hopTotal: number; detail: string };
    }
  | { code: "importTargetUserKeychainFailed"; params: { name: string; detail: string } }
  | { code: "commandFailed"; params: { detail: string } }
  | { code: "migrationFailed"; params: { legacyDir: string; detail: string } }
  | {
      code: "migrationPartialFailed";
      params: { legacyDir: string; newDir: string; failures: string[] };
    }
  | { code: "exportSerializeFailed"; params: { detail: string } }
  | { code: "exportWriteFailed"; params: { path: string; detail: string } }
  | { code: "importReadFailed"; params: { path: string; detail: string } }
  | { code: "importInvalidJson"; params: { detail: string } }
  | { code: "importUnknownFormat"; params: { format: string; expected: string } }
  | {
      code: "importUnsupportedVersion";
      params: { version: number; maxVersion: number };
    }
  | { code: "importEmptyId" }
  | { code: "importEmptyName" }
  | { code: "trayMissing" }
  | { code: "trayUpdateFailed"; params: { detail: string } }
  | { code: "mainWindowMissing" }
  | { code: "setTitleFailed"; params: { detail: string } };

/**
 * Loose form of a backend message at event/command boundaries: values coming
 * over IPC are not statically typed, so runtime validation happens inside
 * renderMsg (unknown codes degrade to the i18next fallback, ultimately the
 * commandFailed code via toMessage).
 */
export interface LocalizedMessage {
  code: Msg["code"];
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
