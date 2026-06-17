import { useState } from "react";
import { submitPassword } from "../api";
import { Overlay } from "./TunnelEditor";

interface Props {
  id: string;
  prompt: string;
  onClose: () => void;
}

export default function PasswordModal({ id, prompt, onClose }: Props) {
  const [password, setPassword] = useState("");
  const [save, setSave] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!password) return;
    setSubmitting(true);
    setError("");
    try {
      await submitPassword(id, password, save);
      onClose();
    } catch (err) {
      setError(String(err));
      setSubmitting(false);
    }
  };

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 380 }}>
        <h3 style={{ marginTop: 0, marginBottom: 4 }}>🔐 需要密码</h3>
        <p style={{ color: "#9ca3af", fontSize: 13, marginBottom: 16 }}>{prompt}</p>

        <form onSubmit={handleSubmit}>
          <input
            type="password"
            autoFocus
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="输入密码"
            style={{
              width: "100%", padding: "10px 12px", fontSize: 14,
              backgroundColor: "#1f2937", color: "#f9fafb",
              border: "1px solid #374151", borderRadius: 6,
              boxSizing: "border-box",
            }}
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
            记住密码（存入系统密钥链）
          </label>

          {error && (
            <div style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{error}</div>
          )}

          <div style={{ display: "flex", gap: 8, marginTop: 16, justifyContent: "flex-end" }}>
            <button
              type="button"
              onClick={onClose}
              style={{
                padding: "8px 16px", backgroundColor: "transparent", color: "#9ca3af",
                border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
              }}
            >
              取消
            </button>
            <button
              type="submit"
              disabled={submitting || !password}
              style={{
                padding: "8px 20px", backgroundColor: "#3b82f6", color: "#fff",
                border: "none", borderRadius: 6, cursor: "pointer", fontSize: 14,
                opacity: submitting || !password ? 0.6 : 1,
              }}
            >
              {submitting ? "提交中…" : "确认"}
            </button>
          </div>
        </form>
      </div>
    </Overlay>
  );
}
