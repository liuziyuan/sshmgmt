import "./App.css";
import { useState, useEffect, useCallback, useRef } from "react";
import {
  listTunnels, connectTunnel, disconnectTunnel, reconnectTunnel,
  reconnectAll, deleteTunnel, onStateChanged, onPasswordRequired, onNotice,
} from "./api";
import type { PasswordRequiredPayload, NoticePayload } from "./api";
import type { TunnelInfo } from "./types";
import TunnelList from "./components/TunnelList";
import TunnelEditor from "./components/TunnelEditor";
import PasswordModal from "./components/PasswordModal";

// ─── Banner (toast) feedback ───────────────────────────────────────────────────
// error / warn banners are persistent — the user must click ✕ to dismiss them.
// success banners auto-dismiss after a few seconds. Multiple banners stack
// instead of overwriting each other, so no error goes unnoticed.
export type BannerLevel = NoticePayload["level"];
interface Banner {
  id: number;
  level: BannerLevel;
  message: string;
}
export type Notify = (level: BannerLevel, message: string) => void;

const AUTO_DISMISS_MS = 6000;

export default function App() {
  const [tunnels, setTunnels] = useState<TunnelInfo[]>([]);
  const [showEditor, setShowEditor] = useState(false);
  const [editTarget, setEditTarget] = useState<TunnelInfo | null>(null);
  const [pendingPassword, setPendingPassword] = useState<PasswordRequiredPayload | null>(null);
  const [banners, setBanners] = useState<Banner[]>([]);
  const bannerIdRef = useRef(0);

  const dismissBanner = useCallback((id: number) => {
    setBanners((prev) => prev.filter((b) => b.id !== id));
  }, []);

  const notify: Notify = useCallback((level, message) => {
    const id = ++bannerIdRef.current;
    setBanners((prev) => [...prev, { id, level, message }]);
    if (level === "success") {
      setTimeout(() => dismissBanner(id), AUTO_DISMISS_MS);
    }
  }, [dismissBanner]);

  // Keep a ref mirror of `tunnels` so the state-changed listener (registered
  // once) can look up a tunnel's display name without going stale.
  const tunnelsRef = useRef<TunnelInfo[]>([]);
  useEffect(() => { tunnelsRef.current = tunnels; }, [tunnels]);

  // Track each tunnel's previous state type so we only pop a banner on the
  // transition INTO "Failed" (not on every re-render / re-emit while it stays
  // failed), and never on reconnect churn.
  const prevStateRef = useRef<Map<string, string>>(new Map());

  const reload = useCallback(async () => {
    try {
      setTunnels(await listTunnels());
    } catch (e) {
      notify("error", `加载隧道列表失败：${e}`);
    }
  }, [notify]);

  useEffect(() => {
    reload();

    const unState = onStateChanged(({ id, state }) => {
      setTunnels((prev) => prev.map((t) => t.config.id === id ? { ...t, state } : t));

      const prevType = prevStateRef.current.get(id);
      prevStateRef.current.set(id, state.type);
      if (state.type === "Failed" && prevType !== "Failed") {
        const name = tunnelsRef.current.find((t) => t.config.id === id)?.config.name ?? id;
        notify("error", `隧道「${name}」连接失败：${state.message}`);
      }
    });

    const unPw = onPasswordRequired((payload) => {
      setPendingPassword(payload);
    });

    const unNotice = onNotice(({ level, message }) => {
      notify(level, message);
    });

    return () => {
      unState.then((fn) => fn());
      unPw.then((fn) => fn());
      unNotice.then((fn) => fn());
    };
  }, [reload, notify]);

  const wrap = (fn: () => Promise<void>) => () =>
    fn().catch((e) => notify("error", String(e)));

  const handleConnect    = (id: string) => wrap(() => connectTunnel(id))();
  const handleDisconnect = (id: string) => wrap(() => disconnectTunnel(id))();
  const handleReconnect  = (id: string) => wrap(() => reconnectTunnel(id))();
  const handleReconnectAll = wrap(reconnectAll);

  const handleDelete = async (id: string) => {
    try { await deleteTunnel(id); await reload(); }
    catch (e) { notify("error", `删除失败：${e}`); }
  };

  // User clicked ✕ / 取消 on the password prompt: give up on this login
  // attempt entirely (disconnect the tunnel) rather than merely hiding the
  // modal — otherwise the backend keeps waiting on the prompt (up to 5 min)
  // and the tunnel is left stuck on yellow "Connecting" with no visible way
  // to tell it's actually dead.
  const handlePasswordCancel = () => {
    if (!pendingPassword) return;
    const { id } = pendingPassword;
    setPendingPassword(null);
    disconnectTunnel(id).catch((e) => notify("error", `取消连接失败：${e}`));
  };

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

      {/* Feedback banners: stack of error/warn (persistent, click ✕) and
          success (auto-dismiss) — newest at the bottom. */}
      {banners.map((b) => (
        <Banner key={b.id} level={b.level} message={b.message} onClose={() => dismissBanner(b.id)} />
      ))}

      <main style={{ padding: "24px" }}>
        <TunnelList
          tunnels={tunnels}
          onConnect={handleConnect}
          onDisconnect={handleDisconnect}
          onReconnect={handleReconnect}
          onEdit={(info) => { setEditTarget(info); setShowEditor(true); }}
          onDelete={handleDelete}
        />

        <footer style={{
          marginTop: 32, paddingTop: 16, borderTop: "1px solid #1f2937",
          fontSize: 12, color: "#6b7280", lineHeight: 1.6,
        }}>
          🔑 连接时若使用用户名密码，可勾选「连接后上传公钥」，下次自动免密连接。
        </footer>
      </main>

      {showEditor && (
        <TunnelEditor
          editTarget={editTarget}
          tunnels={tunnels}
          notify={notify}
          onClose={() => { setShowEditor(false); setEditTarget(null); }}
          onSaved={reload}
        />
      )}

      {pendingPassword && (
        <PasswordModal
          request={pendingPassword}
          notify={notify}
          onClose={() => setPendingPassword(null)}
          onCancel={handlePasswordCancel}
        />
      )}
    </div>
  );
}

// ─── Feedback banner row ───────────────────────────────────────────────────────

const BANNER_STYLE: Record<BannerLevel, { bg: string; border: string; fg: string; icon: string }> = {
  error:   { bg: "#450a0a", border: "#ef4444", fg: "#fca5a5", icon: "⛔" },
  warn:    { bg: "#422006", border: "#f59e0b", fg: "#fcd34d", icon: "⚠" },
  success: { bg: "#052e16", border: "#22c55e", fg: "#86efac", icon: "✅" },
};

function Banner({ level, message, onClose }: { level: BannerLevel; message: string; onClose: () => void }) {
  const s = BANNER_STYLE[level];
  return (
    <div style={{
      backgroundColor: s.bg, borderBottom: `1px solid ${s.border}`,
      padding: "10px 24px", fontSize: 13, color: s.fg,
      display: "flex", justifyContent: "space-between", alignItems: "center",
    }}>
      <span>{s.icon} {message}</span>
      <button onClick={onClose}
        style={{ background: "none", border: "none", color: s.fg, cursor: "pointer", fontSize: 14 }}>
        ✕
      </button>
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
