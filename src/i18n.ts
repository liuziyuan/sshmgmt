import i18next, { type i18n as I18n } from "i18next";
import { initReactI18next } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import zh from "./locales/zh.json";
import en from "./locales/en.json";
import type { LocalizedMessage } from "./types";

export const LANG_STORAGE_KEY = "portunnel.language";
export type Lang = "zh" | "en";
export type LangChoice = "auto" | Lang;

// H3: navigator.language starting with "zh" maps to zh, everything else to
// en; manual choice (localStorage) > system language > default zh (A1).
export function resolveLanguage(): Lang {
  try {
    const saved = window.localStorage.getItem(LANG_STORAGE_KEY);
    if (saved === "zh" || saved === "en") return saved;
  } catch {
    // storage unavailable → fall through to system detection
  }
  return navigator.language?.toLowerCase().startsWith("zh") ? "zh" : "en";
}

/** The raw persisted choice ("auto" | "zh" | "en"); "auto" when unset. */
export function savedLanguageChoice(): LangChoice {
  try {
    const saved = window.localStorage.getItem(LANG_STORAGE_KEY);
    if (saved === "zh" || saved === "en" || saved === "auto") return saved;
  } catch {
    // ignore
  }
  return "auto";
}

void i18next.use(initReactI18next).init({
  resources: { zh: { translation: zh }, en: { translation: en } },
  lng: resolveLanguage(),
  fallbackLng: "zh", // A4: missing keys fall back to zh, never a bare key
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
  initImmediate: false, // synchronous init with bundled resources: first frame already speaks the right language
});

/**
 * Render one backend message in the current language. Safe outside render
 * paths (event callbacks etc.) — always evaluates at call time against the
 * i18next instance, so a stale `t` snapshot can never leak an old language.
 *
 * Two codes need special handling:
 * - hopAuthFailed / pubkey notices distinguish single-hop vs multi-hop via
 *   optional hopIndex/hopTotal params; the two templates live under the same
 *   msg key as an object (`single` / `multi`), keeping the msg.* key set
 *   identical to the Rust enum (check-msg-codes compares one level).
 * - migrationPartialFailed carries `failures` as a string array from the
 *   backend; the join separator is language-specific, so it happens here.
 */
export function renderMsg(m: LocalizedMessage): string {
  const params: Record<string, unknown> = { ...(m.params ?? {}) };
  if (Array.isArray(params.failures)) {
    params.failures = (params.failures as unknown[]).join(
      i18next.t("common.listSeparator"),
    );
  }
  if (
    (m.code === "hopAuthFailed" ||
      m.code === "pubkeyUploaded" ||
      m.code === "pubkeyUploadFailed") &&
    params.hopIndex != null &&
    params.hopTotal != null
  ) {
    return i18next.t(`msg.${m.code}.multi`, params);
  }
  return i18next.t(`msg.${m.code}`, params);
}

/**
 * Normalize anything caught from an invoke rejection into a LocalizedMessage.
 * The backend serializes command errors as `Msg`; non-JSON values (plain
 * strings, DOMException-like errors) degrade to the commandFailed code.
 */
export function toMessage(e: unknown): LocalizedMessage {
  if (e && typeof e === "object" && "code" in e) {
    const code = (e as { code?: unknown }).code;
    if (typeof code === "string") {
      const params = (e as { params?: Record<string, unknown> }).params;
      return { code: code as LocalizedMessage["code"], params };
    }
  }
  return {
    code: "commandFailed",
    params: { detail: typeof e === "string" ? e : String(e) },
  };
}

export default i18next as I18n;

/**
 * Apply a language choice ("auto" re-resolves the system language), persist
 * it, and sync the tray menu + window title via the rebuild commands (A2).
 * Idempotent: applying the current choice is a no-op language-wise and just
 * refreshes the tray/title. Storage failures degrade to an in-memory choice
 * for this session. Invoke failures propagate to the caller — App and
 * SettingsModal surface them as warn banners without blocking the switch.
 */
export async function applyLanguage(choice: LangChoice): Promise<void> {
  try {
    window.localStorage.setItem(LANG_STORAGE_KEY, choice);
  } catch {
    // ignore: session-only choice
  }
  await i18next.changeLanguage(choice === "auto" ? resolveLanguage() : choice);
  const tooltip = i18next.t("app.windowTitle");
  await invoke("set_window_title", { title: tooltip });
  await invoke("rebuild_tray", {
    labels: {
      show: i18next.t("tray.show"),
      reconnectAll: i18next.t("tray.reconnectAll"),
      quit: i18next.t("tray.quit"),
      tooltip,
    },
  });
}
