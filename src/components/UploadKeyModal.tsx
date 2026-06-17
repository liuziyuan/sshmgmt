import { useState } from "react";
import { uploadPubkey } from "../api";
import { Overlay } from "./TunnelEditor";
import type { TunnelInfo } from "../types";

interface Props {
  tunnel: TunnelInfo;
  onClose: () => void;
}

export default function UploadKeyModal({ tunnel, onClose }: Props) {
  const [keyContent, setKeyContent] = useState("");
  const [uploading, setUploading] = useState(false);
  const [result, setResult] = useState<"ok" | "err" | null>(null);
  const [error, setError] = useState("");

  const { config } = tunnel;

  const handleUpload = async () => {
    if (!keyContent.trim()) return;
    setUploading(true);
    setResult(null);
    setError("");
    try {
      await uploadPubkey(config.id, keyContent.trim());
      setResult("ok");
    } catch (e) {
      setError(String(e));
      setResult("err");
    } finally {
      setUploading(false);
    }
  };

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 520 }}>
        <h3 style={{ marginTop: 0, marginBottom: 4 }}>🔑 上传公钥</h3>
        <p style={{ color: "#9ca3af", fontSize: 13, marginBottom: 16 }}>
          上传到跳板机 <code style={{ color: "#60a5fa" }}>{config.jump_host}</code> 的{" "}
          <code style={{ color: "#60a5fa" }}>~/.ssh/authorized_keys</code>
        </p>

        <div style={{ marginBottom: 8 }}>
          <label style={{ fontSize: 12, color: "#9ca3af", marginBottom: 4, display: "block" }}>
            公钥内容（粘贴 ~/.ssh/id_*.pub 的内容）
          </label>
          <textarea
            value={keyContent}
            onChange={(e) => setKeyContent(e.target.value)}
            placeholder="ssh-ed25519 AAAA… 或 ssh-rsa AAAA…"
            style={{
              width: "100%", height: 100, fontFamily: "monospace", fontSize: 12,
              backgroundColor: "#1f2937", color: "#f9fafb",
              border: "1px solid #374151", borderRadius: 6,
              padding: 10, resize: "vertical", boxSizing: "border-box",
            }}
          />
        </div>

        {/* Quick-fill from local file hint */}
        <div style={{ fontSize: 11, color: "#6b7280", marginBottom: 12 }}>
          提示：终端执行 <code>cat ~/.ssh/id_ed25519.pub</code> 并粘贴到上方文本框。
        </div>

        {result === "ok" && (
          <div style={{
            backgroundColor: "#14532d", border: "1px solid #22c55e",
            borderRadius: 6, padding: "8px 12px", fontSize: 13, color: "#86efac", marginBottom: 12,
          }}>
            ✅ 公钥已成功追加到 authorized_keys！
          </div>
        )}
        {result === "err" && (
          <div style={{
            backgroundColor: "#450a0a", border: "1px solid #ef4444",
            borderRadius: 6, padding: "8px 12px", fontSize: 13, color: "#fca5a5", marginBottom: 12,
          }}>
            ❌ 上传失败：{error}
          </div>
        )}

        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button
            onClick={onClose}
            style={{
              padding: "8px 16px", backgroundColor: "transparent", color: "#9ca3af",
              border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
            }}
          >
            关闭
          </button>
          <button
            onClick={handleUpload}
            disabled={uploading || !keyContent.trim()}
            style={{
              padding: "8px 20px", backgroundColor: "#10b981", color: "#fff",
              border: "none", borderRadius: 6, cursor: "pointer", fontSize: 14,
              opacity: uploading || !keyContent.trim() ? 0.6 : 1,
            }}
          >
            {uploading ? "上传中…" : "上传公钥"}
          </button>
        </div>
      </div>
    </Overlay>
  );
}
