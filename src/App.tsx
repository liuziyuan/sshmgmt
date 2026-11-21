import "./App.css";
import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  listTunnels, connectTunnel, disconnectTunnel, reconnectTunnel,
  reconnectAll, deleteTunnel, onStateChanged, onPasswordRequired, onNotice,
  exportTunnels, importTunnels, pickExportPath, pickImportPath,
  getGroupOrder, setGroupOrder as setGroupOrderAPI,
  getCollapsedGroups, setCollapsedGroups as setCollapsedGroupsAPI,
  getStartupWarning,
} from "./api";
import type { PasswordRequiredPayload, NoticePayload } from "./api";
import type { TunnelInfo } from "./types";
import i18n, { applyLanguage, renderMsg, savedLanguageChoice, toMessage } from "./i18n";
import TunnelList from "./components/TunnelList";
import TunnelEditor from "./components/TunnelEditor";
import PasswordModal from "./components/PasswordModal";
import SettingsModal from "./components/SettingsModal";

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
  const { t } = useTranslation();
  const [tunnels, setTunnels] = useState<TunnelInfo[]>([]);
  const [showEditor, setShowEditor] = useState(false);
  const [editTarget, setEditTarget] = useState<TunnelInfo | null>(null);
  const [pendingPassword, setPendingPassword] = useState<PasswordRequiredPayload | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [banners, setBanners] = useState<Banner[]>([]);
  const [groupOrder, setGroupOrder] = useState<string[]>([]);
  const [collapsedGroups, setCollapsedGroups] = useState<string[]>([]);
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
      notify("error", i18n.t("banner.loadTunnelsFailed", { detail: renderMsg(toMessage(e)) }));
    }
    try {
      setGroupOrder(await getGroupOrder());
    } catch (e) {
      notify("error", i18n.t("banner.loadGroupOrderFailed", { detail: renderMsg(toMessage(e)) }));
    }
    try {
      setCollapsedGroups(await getCollapsedGroups());
    } catch (e) {
      notify("error", i18n.t("banner.loadCollapsedFailed", { detail: renderMsg(toMessage(e)) }));
    }
  }, [notify]);

  useEffect(() => {
    reload();
    getStartupWarning()
      .then((w) => {
        if (w) notify("warn", renderMsg(w));
      })
      .catch(() => {});

    // Re-apply the persisted language choice once at mount: idempotent for
    // i18next, but it also rebuilds the tray menu and window title from the
    // backend's zh fallback into the user's actual language (A2/R4).
    applyLanguage(savedLanguageChoice()).catch((e) =>
      notify("warn", renderMsg(toMessage(e)))
    );

    const unState = onStateChanged(({ id, state }) => {
      setTunnels((prev) => prev.map((t) => t.config.id === id ? { ...t, state } : t));

      const prevType = prevStateRef.current.get(id);
      prevStateRef.current.set(id, state.type);
      if (state.type === "Failed" && prevType !== "Failed") {
        const name = tunnelsRef.current.find((t) => t.config.id === id)?.config.name ?? id;
        notify("error", i18n.t("banner.tunnelFailed", { name, reason: renderMsg(state.message) }));
      }
    });

    const unPw = onPasswordRequired((payload) => {
      setPendingPassword(payload);
    });

    const unNotice = onNotice(({ level, message }) => {
      notify(level, renderMsg(message));
    });

    return () => {
      unState.then((fn) => fn());
      unPw.then((fn) => fn());
      unNotice.then((fn) => fn());
    };
  }, [reload, notify]);

  const wrap = (fn: () => Promise<void>) => () =>
    fn().catch((e) => notify("error", renderMsg(toMessage(e))));

  const handleConnect    = (id: string) => wrap(() => connectTunnel(id))();
  const handleDisconnect = (id: string) => wrap(() => disconnectTunnel(id))();
  const handleReconnect  = (id: string) => wrap(() => reconnectTunnel(id))();
  const handleReconnectAll = wrap(reconnectAll);

  const handleDelete = async (id: string) => {
    try { await deleteTunnel(id); await reload(); }
    catch (e) { notify("error", i18n.t("banner.deleteFailed", { detail: renderMsg(toMessage(e)) })); }
  };

  // The export file embeds keychain passwords in PLAINTEXT — remind the user
  // to treat it like a password in the success banner.
  const handleExport = async () => {
    if (tunnels.length === 0) {
      notify("warn", i18n.t("banner.noTunnelsToExport"));
      return;
    }
    try {
      const path = await pickExportPath();
      if (!path) return; // user cancelled the dialog
      const n = await exportTunnels(path);
      notify("success", i18n.t("banner.exportSuccess", { count: n, path }));
    } catch (e) {
      notify("error", i18n.t("banner.exportFailed", { detail: renderMsg(toMessage(e)) }));
    }
  };

  // Merge import: conflicting tunnels (same id or name) are skipped untouched.
  // A dialog cancel (null path) exits silently.
  const handleImport = async () => {
    try {
      const path = await pickImportPath();
      if (!path) return;
      const s = await importTunnels(path);
      await reload();
      if (s.warnings.length > 0) {
        notify("warn", i18n.t("banner.importWarnings", {
          warnings: s.warnings.map(renderMsg).join(i18n.t("common.listSeparator")),
        }));
      }
      if (s.skipped > 0) {
        // warn is persistent (no auto-dismiss) — the skipped-name list needs
        // to stay readable while the user checks it against the tunnel list.
        notify("warn", i18n.t("banner.importSkipped", {
          imported: s.imported,
          skipped: s.skipped,
          names: s.skipped_names.join(i18n.t("common.nameSeparator")),
        }));
      } else {
        notify("success", i18n.t("banner.importSuccess", { imported: s.imported }));
      }
    } catch (e) {
      notify("error", i18n.t("banner.importFailed", { detail: renderMsg(toMessage(e)) }));
    }
  };

  // Group drag reorder: update locally for instant feedback, persist in the
  // background; a persistence failure leaves the local order in place and
  // just surfaces a banner (the next app start reloads the saved order).
  const handleGroupOrderChange = (order: string[]) => {
    setGroupOrder(order);
    setGroupOrderAPI(order).catch((e) =>
      notify("error", i18n.t("banner.saveGroupOrderFailed", { detail: renderMsg(toMessage(e)) }))
    );
  };

  // Collapse state is likewise local-first, persisted in the background.
  const handleCollapsedChange = (groups: string[]) => {
    setCollapsedGroups(groups);
    setCollapsedGroupsAPI(groups).catch((e) =>
      notify("error", i18n.t("banner.saveCollapsedFailed", { detail: renderMsg(toMessage(e)) }))
    );
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
    disconnectTunnel(id).catch((e) =>
      notify("error", i18n.t("banner.cancelConnectFailed", { detail: renderMsg(toMessage(e)) }))
    );
  };

  return (
    <div style={{
      // Fixed-viewport column: header and banners never scroll, only <main>
      // does. This keeps the title/actions bar pinned as a permanent top bar
      // and stops the whole document from scrolling behind it.
      height: "100vh", display: "flex", flexDirection: "column",
      overflow: "hidden", backgroundColor: "#111827", color: "#f9fafb",
      fontFamily: "system-ui, -apple-system, sans-serif",
    }}>
      {/* Header (never scrolls) */}
      <header style={{
        flexShrink: 0,
        backgroundColor: "#1e2433", borderBottom: "1px solid #374151",
        padding: "14px 24px", display: "flex", alignItems: "center",
        justifyContent: "space-between",
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span style={{ fontSize: 20 }}>🔌</span>
          <span style={{ fontSize: 18, fontWeight: 600 }}>{t("app.title")}</span>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button onClick={handleExport} style={btnSecondary}>
            {t("header.export")}
          </button>
          <button onClick={handleImport} style={btnSecondary}>
            {t("header.import")}
          </button>
          <button onClick={handleReconnectAll} style={btnSecondary}>
            {t("header.reconnectAll")}
          </button>
          <button onClick={() => setShowSettings(true)} style={btnSecondary}>
            ⚙ {t("header.settings")}
          </button>
          <button onClick={() => { setEditTarget(null); setShowEditor(true); }} style={btnPrimary}>
            {t("header.add")}
          </button>
        </div>
      </header>

      {/* Feedback banners: stack of error/warn (persistent, click ✕) and
          success (auto-dismiss) — newest at the bottom. */}
      {banners.map((b) => (
        <Banner key={b.id} level={b.level} message={b.message} onClose={() => dismissBanner(b.id)} />
      ))}

      <main style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "24px" }}>
        <TunnelList
          tunnels={tunnels}
          groupOrder={groupOrder}
          onGroupOrderChange={handleGroupOrderChange}
          collapsedGroups={collapsedGroups}
          onCollapsedChange={handleCollapsedChange}
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
          {t("footer.pubkeyHint")}
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

      {showSettings && (
        <SettingsModal notify={notify} onClose={() => setShowSettings(false)} />
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
      flexShrink: 0,
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
