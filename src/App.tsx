import "./App.css";
import { useState, useEffect, useCallback } from "react";
import {
  listTunnels, connectTunnel, disconnectTunnel, reconnectTunnel,
  reconnectAll, deleteTunnel, onStateChanged, onPasswordRequired,
} from "./api";
import type { TunnelInfo } from "./types";
import TunnelList from "./components/TunnelList";
import TunnelEditor from "./components/TunnelEditor";
import PasswordModal from "./components/PasswordModal";
import UploadKeyModal from "./components/UploadKeyModal";

export default function App() {
  const [tunnels, setTunnels] = useState<TunnelInfo[]>([]);
  const [showEditor, setShowEditor] = useState(false);
  const [editTarget, setEditTarget] = useState<TunnelInfo | null>(null);
  const [pendingPassword, setPendingPassword] = useState<{
    id: string; prompt: string;
  } | null>(null);
  const [uploadKeyId, setUploadKeyId] = useState<string | null>(null);
  const [globalError, setGlobalError] = useState("");

  const reload = useCallback(async () => {
    try {
      setTunnels(await listTunnels());
    } catch (e) {
      setGlobalError(String(e));
    }
  }, []);

  useEffect(() => {
    reload();

    const unState = onStateChanged(({ id, state }) => {
      setTunnels((prev) => prev.map((t) => t.config.id === id ? { ...t, state } : t));
    });

    const unPw = onPasswordRequired(({ id, prompt }) => {
      setPendingPassword({ id, prompt });
    });

    return () => {
      unState.then((fn) => fn());
      unPw.then((fn) => fn());
    };
  }, [reload]);

  const wrap = (fn: () => Promise<void>) => () =>
    fn().catch((e) => setGlobalError(String(e)));

  const handleConnect    = (id: string) => wrap(() => connectTunnel(id))();
  const handleDisconnect = (id: string) => wrap(() => disconnectTunnel(id))();
  const handleReconnect  = (id: string) => wrap(() => reconnectTunnel(id))();
  const handleReconnectAll = wrap(reconnectAll);

  const handleDelete = async (id: string) => {
    if (!confirm("确认删除这个隧道？")) return;
    try { await deleteTunnel(id); await reload(); }
    catch (e) { setGlobalError(String(e)); }
  };

  const uploadKeyTunnel = uploadKeyId
    ? tunnels.find((t) => t.config.id === uploadKeyId) ?? null
    : null;

  return (
    <div style={{
      minHeight: "100vh", backgroundColor: "#111827", color: "#f9fafb",
      fontFamily: "system-ui, -apple-system, sans-serif",
    }}>
      {/* Header */}
      <header style={{
        backgroundColor: "#1e2433", borderBottom: "1px solid #374151",
        padding: "14px 24px", display: "flex", alignItems: "center",
        justifyContent: "space-between",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span style={{ fontSize: 20 }}>🔌</span>
          <span style={{ fontSize: 18, fontWeight: 600 }}>SSH 隧道管理器</span>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button onClick={handleReconnectAll} style={btnSecondary}>
            🔄 全部重连
          </button>
          <button onClick={() => { setEditTarget(null); setShowEditor(true); }} style={btnPrimary}>
            + 新增隧道
          </button>
        </div>
      </header>

      {/* Error banner */}
      {globalError && (
        <div style={{
          backgroundColor: "#450a0a", borderBottom: "1px solid #ef4444",
          padding: "10px 24px", fontSize: 13, color: "#fca5a5",
          display: "flex", justifyContent: "space-between", alignItems: "center",
        }}>
          <span>⚠ {globalError}</span>
          <button onClick={() => setGlobalError("")}
            style={{ background: "none", border: "none", color: "#fca5a5", cursor: "pointer" }}>
            ✕
          </button>
        </div>
      )}

      <main style={{ padding: "24px" }}>
        <TunnelList
          tunnels={tunnels}
          onConnect={handleConnect}
          onDisconnect={handleDisconnect}
          onReconnect={handleReconnect}
          onEdit={(info) => { setEditTarget(info); setShowEditor(true); }}
          onDelete={handleDelete}
          onUploadKey={setUploadKeyId}
        />
      </main>

      {showEditor && (
        <TunnelEditor
          editTarget={editTarget}
          tunnels={tunnels}
          onClose={() => { setShowEditor(false); setEditTarget(null); }}
          onSaved={reload}
        />
      )}

      {pendingPassword && (
        <PasswordModal
          id={pendingPassword.id}
          prompt={pendingPassword.prompt}
          onClose={() => setPendingPassword(null)}
        />
      )}

      {uploadKeyTunnel && (
        <UploadKeyModal
          tunnel={uploadKeyTunnel}
          onClose={() => setUploadKeyId(null)}
        />
      )}
    </div>
  );
}

const btnPrimary: React.CSSProperties = {
  padding: "7px 16px", backgroundColor: "#3b82f6", color: "#fff",
  border: "none", borderRadius: 6, cursor: "pointer", fontSize: 14,
};
const btnSecondary: React.CSSProperties = {
  padding: "7px 16px", backgroundColor: "transparent", color: "#9ca3af",
  border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
};
