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
// `code` is a translation key under editor.validate.* — this module stays
// i18n-free; the UI translates at render time (see TunnelEditor ErrorHints).
export interface FormFieldError {
  field: string;
  code: string;
  params?: Record<string, unknown>;
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

// Characters that break shell-words tokenization (POSIX quoting and
// backslash escaping) or the parser's destination/spec splitting. Assembly
// never escapes them, so any of them would silently corrupt or break the
// saved command (review S1). Applies to all four free-text fields.
const FORBIDDEN_QUOTE_BACKSLASH_RE = /['"\\]/;

// Validation rules (baseline B3 + rulings on parser-backed bans):
// - remote host must not contain ":" (parser splits -L spec on ":");
// - jump host must not contain "@" (destination splits on last "@"),
//   nor "[" or "]" (only bracketed-IPv6 input is normalized away on
//   change; any remaining bracket is malformed, review G1);
// - all four free-text fields (jump user/host, remote host, identity
//   path) must not contain quotes or backslash (review S1 / ruling 3);
// - identity path double quotes are part of that same ban.
// Values are trimmed before checks; assembly trims the same way, so the
// command is built from exactly the validated text.
// Error codes are editor.validate.* keys; rule order is unchanged.
export function validateFormState(s: EditorFormState): FormFieldError[] {
  const errors: FormFieldError[] = [];

  const jumpUser = s.jumpUser.trim();
  if (!jumpUser) {
    errors.push({ field: "jumpUser", code: "editor.validate.usernameRequired" });
  } else if (/\s/.test(jumpUser)) {
    errors.push({ field: "jumpUser", code: "editor.validate.usernameNoSpaces" });
  } else if (FORBIDDEN_QUOTE_BACKSLASH_RE.test(jumpUser)) {
    errors.push({ field: "jumpUser", code: "editor.validate.usernameForbiddenChars" });
  }

  const jumpHost = s.jumpHost.trim();
  if (!jumpHost) {
    errors.push({ field: "jumpHost", code: "editor.validate.jumpHostRequired" });
  } else {
    if (/\s/.test(jumpHost)) {
      errors.push({ field: "jumpHost", code: "editor.validate.jumpHostNoSpaces" });
    }
    if (jumpHost.includes("@")) {
      errors.push({ field: "jumpHost", code: "editor.validate.jumpHostNoAtSign" });
    }
    if (FORBIDDEN_QUOTE_BACKSLASH_RE.test(jumpHost)) {
      errors.push({ field: "jumpHost", code: "editor.validate.jumpHostForbiddenChars" });
    }
    if (jumpHost.includes("[") || jumpHost.includes("]")) {
      errors.push({ field: "jumpHost", code: "editor.validate.jumpHostNoBrackets" });
    }
  }

  if (parsePort(s.jumpPort.trim()) === null) {
    errors.push({ field: "jumpPort", code: "editor.validate.portRange" });
  }

  if (s.forwards.length === 0) {
    errors.push({ field: "forwards", code: "editor.validate.forwardsRequired" });
  }

  s.forwards.forEach((f, i) => {
    if (parsePort(f.localPort.trim()) === null) {
      errors.push({
        field: `forwards[${i}].localPort`,
        code: "editor.validate.portRange",
      });
    }
    const remoteHost = f.remoteHost.trim();
    if (!remoteHost) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        code: "editor.validate.remoteHostRequired",
      });
    } else if (/\s/.test(remoteHost)) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        code: "editor.validate.remoteHostNoSpaces",
      });
    } else if (remoteHost.includes(":")) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        code: "editor.validate.remoteHostNoColon",
      });
    } else if (FORBIDDEN_QUOTE_BACKSLASH_RE.test(remoteHost)) {
      errors.push({
        field: `forwards[${i}].remoteHost`,
        code: "editor.validate.remoteHostForbiddenChars",
      });
    }
    if (parsePort(f.remotePort.trim()) === null) {
      errors.push({
        field: `forwards[${i}].remotePort`,
        code: "editor.validate.portRange",
      });
    }
  });

  if (FORBIDDEN_QUOTE_BACKSLASH_RE.test(s.identityFile)) {
    errors.push({ field: "identityFile", code: "editor.validate.identityFileForbiddenChars" });
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

// True when the user hasn't touched any field that carries intent. A pristine
// form (fresh "new tunnel" dialog) switching to the command tab is a normal
// move, not a failure: there is simply nothing to convert yet.
export function isFormBlank(s: EditorFormState): boolean {
  return (
    s.jumpUser.trim() === "" &&
    s.jumpHost.trim() === "" &&
    !s.bindAll &&
    s.identityFile.trim() === "" &&
    s.forwards.every((f) =>
      f.localPort.trim() === "" &&
      f.remoteHost.trim() === "" &&
      f.remotePort.trim() === "",
    )
  );
}

// Back-fill from an existing config (edit mode). Structured fields only,
// never re-parsing raw_command; ports converted with String(); jump host
// goes through stripHostBrackets defensively so a legacy bracketed value
// is stored as the bare address the form expects (review G1).
export function configToFormState(cfg: TunnelConfig): EditorFormState {
  return {
    jumpUser: cfg.jump_user,
    jumpHost: stripHostBrackets(cfg.jump_host),
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
