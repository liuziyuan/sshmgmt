// Pure helpers for the tunnel editor form mode: form state shape,
// validation, ssh command assembly and config back-fill.
// No React dependency here; TunnelEditor.tsx wires these to inputs.

import type { TunnelConfig } from "../types.ts";

// One forward row in the form. Ports are strings to keep controlled inputs
// free of intermediate NaN states; validation converts to numbers.
export interface ForwardRow {
  localPort: string;
  remoteHost: string;
  remotePort: string;
}

export interface EditorFormState {
  jumpUser: string;
  jumpHost: string; // bare address; IPv6 without brackets
  jumpPort: string;
  forwards: ForwardRow[];
  bindAll: boolean;
  identityFile: string; // empty = not used
}

// Field-located validation error so the UI can render per-field red hints.
export interface FormFieldError {
  field: string;
  message: string;
}

// Assembly never returns a half-broken command: invalid input yields
// ok:false with the same field errors as validation.
export type FormToCommandResult =
  | { ok: true; command: string }
  | { ok: false; errors: FormFieldError[] };

export const EMPTY_FORM_STATE: EditorFormState = {
  jumpUser: "",
  jumpHost: "",
  jumpPort: "22",
  forwards: [{ localPort: "", remoteHost: "", remotePort: "" }],
  bindAll: false,
  identityFile: "",
};

// Strip surrounding brackets so the form stores the bare IPv6 address;
// assembly adds brackets back when the host contains ":" (round-trip safe
// because the parser strips brackets only for bracketed hosts).
export function stripHostBrackets(host: string): string {
  if (host.startsWith("[") && host.endsWith("]")) {
    return host.slice(1, -1);
  }
  return host;
}

// Digits only, integer, 1..65535. Returns null when invalid.
function parsePort(raw: string): number | null {
  if (!/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 1 || n > 65535) return null;
  return n;
}

// Validation rules (baseline B3 + ruling on parser-backed bans):
// - remote host must not contain ":" (parser splits -L spec on ":");
// - jump host must not contain "@" (destination splits on last "@");
// - identity file must not contain double quotes (shell-words quote handling).
// Values are trimmed before checks; assembly trims the same way, so the
// command is built from exactly the validated text.
export function validateFormState(s: EditorFormState): FormFieldError[] {
  const errors: FormFieldError[] = [];

  const jumpUser = s.jumpUser.trim();
  if (!jumpUser) {
    errors.push({ field: "jumpUser", message: "用户名不能为空" });
  } else if (/\s/.test(jumpUser)) {
    errors.push({ field: "jumpUser", message: "用户名不能包含空格" });
  }

  const jumpHost = s.jumpHost.trim();
  if (!jumpHost) {
    errors.push({ field: "jumpHost", message: "跳板机地址不能为空" });
  } else {
    if (/\s/.test(jumpHost)) {
      errors.push({ field: "jumpHost", message: "跳板机地址不能包含空格" });
    }
    if (jumpHost.includes("@")) {
      errors.push({ field: "jumpHost", message: "跳板机地址不能包含 @" });
    }
  }

  if (parsePort(s.jumpPort.trim()) === null) {
    errors.push({ field: "jumpPort", message: "端口必须是 1–65535 的数字" });
  }

  if (s.forwards.length === 0) {
    errors.push({ field: "forwards", message: "至少需要一条转发规则" });
  }

  s.forwards.forEach((f, i) => {
    if (parsePort(f.localPort.trim()) === null) {
      errors.push({
        field: `forwards[${i}].localPort`,
        message: "端口必须是 1–65535 的数字",
      });
    }
    const remoteHost = f.remoteHost.trim();
    if (!remoteHost) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        message: "目标主机不能为空",
      });
    } else if (/\s/.test(remoteHost)) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        message: "目标主机不能包含空格",
      });
    } else if (remoteHost.includes(":")) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        message: "目标主机暂不支持含冒号的地址（如裸 IPv6）",
      });
    }
    if (parsePort(f.remotePort.trim()) === null) {
      errors.push({
        field: `forwards[${i}].remotePort`,
        message: "端口必须是 1–65535 的数字",
      });
    }
  });

  if (s.identityFile.includes('"')) {
    errors.push({ field: "identityFile", message: "私钥路径不能包含双引号" });
  }

  return errors;
}

// Assemble the canonical command:
//   ssh [-g] [-p PORT] [-i PATH] -L SPEC1 [-L SPEC2 ...] USER@HOST
// - "-p" only when the port differs from the parser default 22;
// - identity path with spaces is wrapped in double quotes (a raw double
//   quote inside the path is rejected by validation);
// - jump host containing ":" is bracketed for IPv6 round-trip safety.
export function formToCommand(s: EditorFormState): FormToCommandResult {
  const errors = validateFormState(s);
  if (errors.length > 0) {
    return { ok: false, errors };
  }

  const parts: string[] = ["ssh"];

  if (s.bindAll) {
    parts.push("-g");
  }

  const jumpPort = s.jumpPort.trim();
  if (Number(jumpPort) !== 22) {
    parts.push("-p", jumpPort);
  }

  const identityFile = s.identityFile.trim();
  if (identityFile) {
    parts.push(
      "-i",
      /\s/.test(identityFile) ? `"${identityFile}"` : identityFile,
    );
  }

  for (const f of s.forwards) {
    parts.push(
      "-L",
      `${f.localPort.trim()}:${f.remoteHost.trim()}:${f.remotePort.trim()}`,
    );
  }

  const jumpHost = s.jumpHost.trim();
  const hostPart = jumpHost.includes(":") ? `[${jumpHost}]` : jumpHost;
  parts.push(`${s.jumpUser.trim()}@${hostPart}`);

  return { ok: true, command: parts.join(" ") };
}

// Back-fill from an existing config (edit mode). Structured fields only,
// never re-parsing raw_command; ports converted with String().
export function configToFormState(cfg: TunnelConfig): EditorFormState {
  return {
    jumpUser: cfg.jump_user,
    jumpHost: cfg.jump_host,
    jumpPort: String(cfg.jump_port),
    forwards: cfg.forwards.map((f) => ({
      localPort: String(f.local_port),
      remoteHost: f.remote_host,
      remotePort: String(f.remote_port),
    })),
    bindAll: cfg.bind_all,
    identityFile: cfg.identity_file ?? "",
  };
}
