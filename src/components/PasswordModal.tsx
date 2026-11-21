import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { submitPassword, listPublicKeys } from "../api";
import type { PasswordRequiredPayload, PublicKeyInfo } from "../api";
import type { Notify } from "../App";
import { renderMsg, toMessage } from "../i18n";
import { Overlay } from "./TunnelEditor";

interface Props {
  request: PasswordRequiredPayload;
  notify: Notify;
  onClose: () => void;
  /** User explicitly gave up on this login (✕ / Cancel) — the tunnel connection
   * attempt should be aborted, not just the modal hidden (otherwise the
   * backend keeps waiting on this prompt up to its 5-minute timeout and the
   * tunnel stays stuck yellow "Connecting" with no way to tell it's dead). */
  onCancel: () => void;
}

export default function PasswordModal({ request, notify, onClose, onCancel }: Props) {
  const { t } = useTranslation();
  const { id, prompt, layer, host, needUsername, hopIndex, hopTotal } = request;
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [save, setSave] = useState(true);
  const [upload, setUpload] = useState(true);
  const [keys, setKeys] = useState<PublicKeyInfo[]>([]);
  const [selectedKey, setSelectedKey] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    listPublicKeys()
      .then((ks) => {
        setKeys(ks);
        // Default-select the first key that has a paired private key (only such
        // a key can actually make you passwordless), else the first listed.
        const usable = ks.find((k) => k.has_private) ?? ks[0];
        if (usable) {
          setSelectedKey(usable.path);
        } else {
          setUpload(false);
        }
      })
      .catch(() => setUpload(false));
  }, []);

  const canUpload = keys.length > 0;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!password) return;
    if (needUsername && !username.trim()) return;
    setSubmitting(true);
    setError("");
    try {
      const pubkeyPath = upload && canUpload ? selectedKey : null;
      await submitPassword(
        id, password, save, pubkeyPath,
        needUsername ? username.trim() : undefined,
      );
      onClose();
    } catch (err) {
      const msg = renderMsg(toMessage(err));
      setError(msg);
      notify("error", t("pw.submitFailed", { detail: msg }));
      setSubmitting(false);
    }
  };

  const inputStyle: React.CSSProperties = {
    width: "100%", padding: "10px 12px", fontSize: 14,
    backgroundColor: "#1f2937", color: "#f9fafb",
    border: "1px solid #374151", borderRadius: 6,
    boxSizing: "border-box",
  };

  const multiHop = layer === "jump" && hopTotal !== undefined && hopTotal > 1;
  const title = layer === "target"
    ? t("pw.targetTitle")
    : multiHop
      ? t("pw.hopTitle", { hopIndex, hopTotal, host })
      : t("pw.needPwTitle");
  const hostLabel = layer === "target" ? t("pw.targetHostLabel") : t("pw.jumpHostLabel");

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 400 }}>
        <h3 style={{ marginTop: 0, marginBottom: 4 }}>{title}</h3>
        <p style={{ color: "#9ca3af", fontSize: 13, marginBottom: 4 }}>{renderMsg(prompt)}</p>
        <p style={{ color: "#6b7280", fontSize: 12, marginBottom: 16 }}>
          {hostLabel}<code style={{ color: "#60a5fa" }}>{host}</code>
        </p>

        <form onSubmit={handleSubmit}>
          {needUsername && (
            <input
              type="text"
              autoFocus
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder={t("pw.usernamePlaceholder")}
              style={{ ...inputStyle, marginBottom: 10 }}
            />
          )}
          <input
            type="password"
            autoFocus={!needUsername}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={t("pw.passwordPlaceholder")}
            style={inputStyle}
          />

          <label style={{
            display: "flex", alignItems: "center", gap: 8,
            marginTop: 12, cursor: "pointer", fontSize: 13, color: "#d1d5db",
          }}>
            <input
              type="checkbox"
              checked={save}
              onChange={(e) => setSave(e.target.checked)}
            />
            {t("pw.savePassword")}
          </label>

          <label style={{
            display: "flex", alignItems: "center", gap: 8,
            marginTop: 8, fontSize: 13,
            color: canUpload ? "#d1d5db" : "#6b7280",
            cursor: canUpload ? "pointer" : "not-allowed",
          }}>
            <input
              type="checkbox"
              checked={upload}
              disabled={!canUpload}
              onChange={(e) => setUpload(e.target.checked)}
            />
            {t("pw.uploadPubkey")}
          </label>

          {canUpload && upload && (
            <div style={{ marginTop: 8 }}>
              <select
                value={selectedKey}
                onChange={(e) => setSelectedKey(e.target.value)}
                style={{ ...inputStyle, padding: "8px 10px" }}
              >
                {keys.map((k) => (
                  <option key={k.path} value={k.path}>
                    {k.name}{k.has_private ? "" : t("pw.noPrivateKeySuffix")}
                  </option>
                ))}
              </select>
              {(() => {
                const sel = keys.find((k) => k.path === selectedKey);
                if (!sel) return null;
                return (
                  <div style={{
                    marginTop: 6, fontSize: 11, color: "#6b7280",
                    fontFamily: "monospace", wordBreak: "break-all",
                    lineHeight: 1.5,
                  }}>
                    {sel.content}
                  </div>
                );
              })()}
            </div>
          )}
          {!canUpload && (
            <div style={{ color: "#6b7280", fontSize: 11, marginTop: 4 }}>
              {t("pw.noPubkeyFound")}
            </div>
          )}

          {error && (
            <div style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{error}</div>
          )}

          <div style={{ display: "flex", gap: 8, marginTop: 16, justifyContent: "flex-end" }}>
            <button
              type="button"
              onClick={onCancel}
              style={{
                padding: "8px 16px", backgroundColor: "transparent", color: "#9ca3af",
                border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
              }}
            >
              {t("common.cancel")}
            </button>
            <button
              type="submit"
              disabled={submitting || !password || (needUsername && !username.trim())}
              style={{
                padding: "8px 20px", backgroundColor: "#3b82f6", color: "#fff",
                border: "none", borderRadius: 6, cursor: "pointer", fontSize: 14,
                opacity: submitting || !password || (needUsername && !username.trim()) ? 0.6 : 1,
              }}
            >
              {submitting ? t("pw.submitting") : t("common.confirm")}
            </button>
          </div>
        </form>
      </div>
    </Overlay>
  );
}
