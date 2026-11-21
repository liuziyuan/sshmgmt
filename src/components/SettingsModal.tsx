import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { Notify } from "../App";
import { applyLanguage, savedLanguageChoice, type LangChoice } from "../i18n";
import { renderMsg, toMessage } from "../i18n";
import { Overlay } from "./TunnelEditor";

interface Props {
  notify: Notify;
  onClose: () => void;
}

// One of three radio options. "auto" (default) re-resolves the system
// language on every startup; zh/en pin the choice.
const CHOICES: LangChoice[] = ["auto", "zh", "en"];

/**
 * Settings dialog (G5/D3): language switch applies instantly (no save
 * button); the "check for updates" section is a disabled placeholder for
 * the separate auto-update proposal (N6).
 */
export default function SettingsModal({ notify, onClose }: Props) {
  const { t } = useTranslation();
  const [choice, setChoice] = useState<LangChoice>(() => savedLanguageChoice());

  const pick = (c: LangChoice) => {
    setChoice(c);
    applyLanguage(c).catch((e) => notify("warn", renderMsg(toMessage(e))));
  };

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 360 }}>
        <h3 style={{ marginTop: 0, marginBottom: 16 }}>{t("settings.title")}</h3>

        <div style={{ fontSize: 13, color: "#9ca3af", marginBottom: 8 }}>
          {t("settings.language")}
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 16 }}>
          {CHOICES.map((c) => (
            <label
              key={c}
              style={{
                display: "flex", alignItems: "center", gap: 8,
                fontSize: 14, color: "#d1d5db", cursor: "pointer",
              }}
            >
              <input
                type="radio"
                name="language"
                checked={choice === c}
                onChange={() => pick(c)}
              />
              {t(`settings.${c}`)}
            </label>
          ))}
        </div>

        <div style={{ fontSize: 13, color: "#9ca3af", marginBottom: 8 }}>
          {t("settings.checkUpdate")}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 16 }}>
          <button
            disabled
            style={{
              padding: "7px 16px", backgroundColor: "#1f2937", color: "#6b7280",
              border: "1px solid #374151", borderRadius: 6, fontSize: 13,
              cursor: "not-allowed",
            }}
          >
            {t("settings.checkUpdate")}
          </button>
          <span style={{ fontSize: 12, color: "#6b7280" }}>{t("settings.comingSoon")}</span>
        </div>

        <div style={{ display: "flex", justifyContent: "flex-end" }}>
          <button
            onClick={onClose}
            style={{
              padding: "8px 20px", backgroundColor: "transparent", color: "#9ca3af",
              border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
            }}
          >
            {t("common.cancel")}
          </button>
        </div>
      </div>
    </Overlay>
  );
}
