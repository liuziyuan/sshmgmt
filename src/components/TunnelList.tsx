import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { resolveHops, type TunnelInfo, type TunnelState } from "../types";
import { renderMsg } from "../i18n";

interface Props {
  tunnels: TunnelInfo[];
  groupOrder: string[];
  onGroupOrderChange: (order: string[]) => void;
  collapsedGroups: string[];
  onCollapsedChange: (groups: string[]) => void;
  onConnect: (id: string) => void;
  onDisconnect: (id: string) => void;
  onReconnect: (id: string) => void;
  onEdit: (info: TunnelInfo) => void;
  onDelete: (id: string) => void;
}

const STATE_COLOR: Record<string, string> = {
  Connected:        "#22c55e",
  Connecting:       "#f59e0b",
  Reconnecting:     "#f59e0b",
  Disconnected:     "#374151",
  PasswordRequired: "#3b82f6",
  Failed:           "#ef4444",
};

// Coarse filter buckets shown as chips; finer states share a bucket.
type StateFilter = "all" | "connected" | "connecting" | "failed" | "disconnected";

// Labels resolve through i18n at render time so a language switch re-renders
// them; the bucket keys themselves are stable identifiers.
const FILTER_KEYS: StateFilter[] = ["all", "connected", "connecting", "failed", "disconnected"];

const FILTER_COLOR: Record<StateFilter, string> = {
  all: "#9ca3af",
  connected: "#22c55e",
  connecting: "#f59e0b",
  failed: "#ef4444",
  disconnected: "#6b7280",
};

function bucketOf(state: TunnelState): StateFilter {
  switch (state.type) {
    case "Connected": return "connected";
    case "Connecting":
    case "Reconnecting":
    case "PasswordRequired": return "connecting";
    case "Failed": return "failed";
    default: return "disconnected";
  }
}

const UNGROUPED_KEY = "\u0000ungrouped";

function StateLight({ state }: { state: TunnelState }) {
  const { t } = useTranslation();
  const color = STATE_COLOR[state.type] ?? "#374151";
  const title =
    state.type === "Failed"
      ? t("state.failedTooltip", { reason: renderMsg(state.message) })
      : t(`state.${state.type}`);
  return (
    <span title={title} style={{
      width: 10, height: 10, borderRadius: "50%",
      backgroundColor: color, display: "inline-block",
      boxShadow: state.type === "Connected" ? `0 0 6px ${color}` : "none",
    }} />
  );
}

function Tag({ label, color }: { label: string; color: string }) {
  return (
    <span style={{
      display: "inline-block", fontSize: 11, padding: "1px 7px",
      borderRadius: 10, border: `1px solid ${color}33`,
      color, backgroundColor: `${color}11`,
    }}>
      {label}
    </span>
  );
}

/** Case-insensitive substring match across every user-searchable field. */
function matchesQuery(t: TunnelInfo, q: string): boolean {
  const fields = [
    t.config.name,
    t.config.group,
    // Search environments case-insensitively in both directions.
    t.config.environment?.toUpperCase(),
    t.config.jump_host,
    // Every hop in the chain is searchable (single-hop configs synthesize
    // their hop from the legacy fields above, which stays as a fallback).
    ...resolveHops(t.config).flatMap((h) => [h.host, h.user]),
    ...t.config.forwards.map((f) => String(f.local_port)),
    // Dynamic rules carry ""/0 placeholders and Unknown kinds have unclear
    // field semantics — only Local and Remote targets contribute.
    ...t.config.forwards
      .filter((f) => f.kind === "local" || f.kind === "remote")
      .flatMap((f) => [f.remote_host, String(f.remote_port)]),
  ];
  return fields.some((s) => s && s.toLowerCase().includes(q));
}

export default function TunnelList({
  tunnels, groupOrder, onGroupOrderChange,
  collapsedGroups, onCollapsedChange,
  onConnect, onDisconnect, onReconnect, onEdit, onDelete,
}: Props) {
  const { t } = useTranslation();
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [stateFilter, setStateFilter] = useState<StateFilter>("all");
  // Active drag-and-drop group keys (HTML5 DnD state, not persisted).
  const [dragKey, setDragKey] = useState<string | null>(null);
  const [dropKey, setDropKey] = useState<string | null>(null);

  // All hooks must run before any early return — on the very first render the
  // tunnel list is still empty (async load), so an early return here would
  // skip hooks and crash React when the data arrives.
  const q = query.trim().toLowerCase();
  const filtering = q !== "" || stateFilter !== "all";
  // Collapsed group keys come from the parent (persisted across restarts).
  // While a query or filter is active every group is force-expanded (matches
  // would otherwise hide inside collapsed groups); the user's collapse
  // choices come back once the query/filter is cleared.
  const collapsed = useMemo(
    () => new Set(collapsedGroups),
    [collapsedGroups],
  );

  const filtered = useMemo(
    () => tunnels.filter((t) =>
      (q === "" || matchesQuery(t, q)) &&
      (stateFilter === "all" || bucketOf(t.state) === stateFilter)),
    [tunnels, q, stateFilter],
  );

  const counts = useMemo(() => {
    const c: Record<StateFilter, number> = {
      all: tunnels.length, connected: 0, connecting: 0, failed: 0, disconnected: 0,
    };
    for (const t of tunnels) c[bucketOf(t.state)]++;
    return c;
  }, [tunnels]);

  const groups = useMemo(() => {
    // Sort rows: group → environment (null last) → name, then bucket by
    // group name (ungrouped uses a sentinel key starting with \u0000, which
    // users can't type, so a real group can't collide with it).
    const sorted = [...filtered].sort((a, b) => {
      const ga = a.config.group ?? "￿";
      const gb = b.config.group ?? "￿";
      if (ga !== gb) return ga.localeCompare(gb);
      const ea = a.config.environment ?? "￿";
      const eb = b.config.environment ?? "￿";
      if (ea !== eb) return ea.localeCompare(eb);
      return a.config.name.localeCompare(b.config.name);
    });
    const map = new Map<string, TunnelInfo[]>();
    for (const t of sorted) {
      const key = t.config.group ?? UNGROUPED_KEY;
      if (!map.has(key)) map.set(key, []);
      map.get(key)!.push(t);
    }
    // Order the group buckets themselves: saved groupOrder first (by index),
    // unlisted groups after (alphabetical), "ungrouped" always last.
    const rank = (key: string) => {
      if (key === UNGROUPED_KEY) return Infinity;
      const i = groupOrder.indexOf(key);
      return i === -1 ? Infinity - 1 : i;
    };
    const ordered = new Map(
      Array.from(map.entries()).sort((a, b) => {
        const ra = rank(a[0]), rb = rank(b[0]);
        if (ra !== rb) return ra - rb;
        return a[0].localeCompare(b[0]);
      })
    );
    return ordered;
  }, [filtered, groupOrder]);

  if (tunnels.length === 0) {
    return (
      <div style={{ textAlign: "center", padding: "60px 0", color: "#6b7280" }}>
        <div style={{ fontSize: 40, marginBottom: 12 }}>🔌</div>
        <div>{t("list.empty")}</div>
      </div>
    );
  }

  const isActive = (t: TunnelInfo) =>
    ["Connected", "Connecting", "Reconnecting", "PasswordRequired"].includes(t.state.type);

  const toggleGroup = (key: string) => {
    const next = new Set(collapsedGroups);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    onCollapsedChange(Array.from(next));
  };

  // Expand/collapse every group at once. Only groups currently shown are
  // affected, so filtering doesn't touch unrelated groups' state.
  const allGroupKeys = () => Array.from(groups.keys());
  const anyExpanded = allGroupKeys().some((k) => !collapsed.has(k));
  const expandAll = () => onCollapsedChange(
    Array.from(new Set(collapsedGroups.filter((k) => !allGroupKeys().includes(k))))
  );
  const collapseAll = () => onCollapsedChange(
    Array.from(new Set([...collapsedGroups, ...allGroupKeys()]))
  );

  // Reordering is disabled while filtering — the saved order describes the
  // full view, and dragging inside a filtered view is ambiguous.
  const canDrag = !filtering;

  const handleDrop = (targetKey: string) => {
    if (!dragKey || dragKey === targetKey) return;
    // New order = the currently displayed group sequence with dragKey moved
    // in front of targetKey. Groups outside the saved order get fixed into
    // it as a side effect, which keeps the file self-consistent.
    const keys = Array.from(groups.keys());
    const from = keys.indexOf(dragKey);
    if (from === -1) return;
    keys.splice(from, 1);
    const to = keys.indexOf(targetKey);
    keys.splice(to === -1 ? keys.length : to, 0, dragKey);
    // The ungrouped bucket always stays last; strip it from the saved list.
    onGroupOrderChange(keys.filter((k) => k !== UNGROUPED_KEY));
  };

  const renderRow = (info: TunnelInfo) => {
    const hops = resolveHops(info.config);
    return (
    <tr key={info.config.id} style={{ borderBottom: "1px solid #1f2937" }}>
      <td style={td}>
        {info.config.group
          ? <Tag label={info.config.group} color="#a78bfa" />
          : <span style={{ color: "#4b5563", fontSize: 12 }}>—</span>}
      </td>
      <td style={td}>
        {info.config.environment
          ? <Tag label={info.config.environment.toUpperCase()} color="#60a5fa" />
          : <span style={{ color: "#4b5563", fontSize: 12 }}>—</span>}
      </td>
      <td style={td}>{info.config.name}</td>
      <td style={td}>
        {/* Keys are indices: a Remote rule's local_port is a server-side port
            and may collide with another rule's local port. */}
        {info.config.forwards.map((f, i) => (
          <div key={i} style={{ fontSize: 13 }}>
            {f.kind === "remote" ? (
              <>
                <span style={{ color: "#60a5fa" }}>
                  {t("forward.remote", {
                    bind: f.remote_bind ? `${f.remote_bind}:` : "",
                    port: f.local_port,
                  })}
                </span>
                {f.local_port === 0 && (
                  <span style={{ color: "#9ca3af", fontSize: 11 }}>
                    {t("forward.dynamicAlloc")}
                  </span>
                )}
              </>
            ) : f.kind === "local" || f.kind === "dynamic" ? (
              <span style={{ color: "#60a5fa" }}>:{f.local_port}</span>
            ) : (
              <span title={t("forward.unknown")} style={{ color: "#6b7280" }}>—</span>
            )}
          </div>
        ))}
      </td>
      <td style={td}>
        {info.config.forwards.map((f, i) => (
          <div key={i} style={{ fontSize: 12, color: "#d1d5db" }}>
            {f.kind === "dynamic"
              ? t("forward.socks")
              : f.kind === "local" || f.kind === "remote"
                ? `${f.remote_host}:${f.remote_port}`
                : t("forward.unknown")}
          </div>
        ))}
      </td>
      <td style={td}>
        <span style={{ fontSize: 12 }}>
          {hops.length > 0
            ? hops.map((h, i) => (
                <span key={i}>
                  {i > 0 ? " → " : ""}
                  {h.user}@{h.host}
                  {h.port !== 22 ? `:${h.port}` : ""}
                </span>
              ))
            : "—"}
        </span>
      </td>
      <td style={td}>
        <StateLight state={info.state} />
      </td>
      <td style={{ ...td, whiteSpace: "nowrap" }}>
        {!isActive(info) ? (
          <Btn onClick={() => onConnect(info.config.id)} color="#3b82f6">{t("action.connect")}</Btn>
        ) : (
          <Btn onClick={() => onDisconnect(info.config.id)} color="#6b7280">{t("action.disconnect")}</Btn>
        )}
        <Btn onClick={() => onReconnect(info.config.id)} color="#f59e0b">{t("action.reconnect")}</Btn>
        <Btn onClick={() => onEdit(info)} color="#8b5cf6">{t("action.edit")}</Btn>
        {confirmId === info.config.id ? (
          <>
            <Btn onClick={() => { onDelete(info.config.id); setConfirmId(null); }} color="#dc2626">{t("action.confirmDelete")}</Btn>
            <Btn onClick={() => setConfirmId(null)} color="#6b7280">{t("action.cancel")}</Btn>
          </>
        ) : (
          <Btn onClick={() => setConfirmId(info.config.id)} color="#ef4444">{t("action.delete")}</Btn>
        )}
      </td>
    </tr>
    );
  };

  const tableHeader = (
    <thead>
      <tr style={{ borderBottom: "1px solid #374151", color: "#9ca3af", fontSize: 12 }}>
        <th style={th}>{t("table.project")}</th>
        <th style={th}>{t("table.environment")}</th>
        <th style={th}>{t("table.name")}</th>
        <th style={th}>{t("table.listenPort")}</th>
        <th style={th}>{t("table.target")}</th>
        <th style={th}>{t("table.jump")}</th>
        <th style={th}>{t("table.state")}</th>
        <th style={th}>{t("table.actions")}</th>
      </tr>
    </thead>
  );

  return (
    <div>
      {/* Toolbar: search box + state filter chips */}
      <div style={{
        display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap",
        marginBottom: 16,
      }}>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("list.searchPlaceholder")}
          style={{
            flex: 1, minWidth: 220, maxWidth: 420, padding: "7px 12px",
            fontSize: 13, color: "#f9fafb", backgroundColor: "#1f2937",
            border: "1px solid #374151", borderRadius: 6, outline: "none",
          }}
        />
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {FILTER_KEYS.map((f) => (
            <button
              key={f}
              onClick={() => setStateFilter(f)}
              style={{
                padding: "4px 10px", fontSize: 12, borderRadius: 12,
                cursor: "pointer",
                color: stateFilter === f ? "#111827" : FILTER_COLOR[f],
                backgroundColor: stateFilter === f ? FILTER_COLOR[f] : "transparent",
                border: `1px solid ${FILTER_COLOR[f]}66`,
              }}
            >
              {t(`filter.${f}`)}({counts[f]})
            </button>
          ))}
          <button
            onClick={anyExpanded ? collapseAll : expandAll}
            title={anyExpanded ? t("list.collapseAllTitle") : t("list.expandAllTitle")}
            style={{
              padding: "4px 10px", fontSize: 12, borderRadius: 12,
              cursor: "pointer", color: "#9ca3af", backgroundColor: "transparent",
              border: "1px solid #374151",
            }}
          >
            {anyExpanded ? t("list.collapseAll") : t("list.expandAll")}
          </button>
        </div>
      </div>

      {filtered.length === 0 ? (
        <div style={{ textAlign: "center", padding: "40px 0", color: "#6b7280" }}>
          {t("list.noMatch")}
        </div>
      ) : (
        Array.from(groups.entries()).map(([key, items]) => {
          const isUngrouped = key === UNGROUPED_KEY;
          const label = isUngrouped ? t("list.ungrouped") : key;
          const connected = items.filter((t) => t.state.type === "Connected").length;
          const isCollapsed = !filtering && collapsed.has(key);
          const isDropTarget = dropKey === key && dragKey !== key && dragKey !== null;
          return (
            <div key={key} style={{ marginBottom: 16 }}>
              <div
                onClick={() => toggleGroup(key)}
                onDragOver={(e) => {
                  if (!dragKey || dragKey === key) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  setDropKey(key);
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  handleDrop(key);
                  setDropKey(null);
                }}
                style={{
                  display: "flex", alignItems: "center", gap: 10,
                  padding: "8px 12px", cursor: "pointer", userSelect: "none",
                  backgroundColor: "#1e2433", borderRadius: 6,
                  borderBottom: "1px solid #374151",
                  ...(isDropTarget ? {
                    border: "1px solid #3b82f6",
                    boxShadow: "0 0 0 1px #3b82f6",
                  } : {}),
                  opacity: dragKey === key ? 0.4 : 1,
                }}
              >
                <span
                  title={canDrag ? t("list.dragHint") : t("list.dragDisabledHint")}
                  draggable={canDrag}
                  onDragStart={(e) => {
                    if (!canDrag) return;
                    setDragKey(key);
                    e.dataTransfer.effectAllowed = "move";
                    // Required for Firefox; harmless elsewhere.
                    e.dataTransfer.setData("text/plain", key);
                  }}
                  onDragEnd={() => { setDragKey(null); setDropKey(null); }}
                  style={{
                    fontSize: 12, color: canDrag ? "#6b7280" : "#374151",
                    width: 14, flexShrink: 0,
                    cursor: canDrag ? "grab" : "not-allowed",
                    lineHeight: 1,
                  }}
                >
                  ⠿
                </span>
                <span style={{ fontSize: 10, color: "#9ca3af", width: 12 }}>
                  {isCollapsed ? "▶" : "▼"}
                </span>
                {isUngrouped
                  ? <span style={{ fontSize: 13, color: "#6b7280" }}>{label}</span>
                  : <Tag label={label} color="#a78bfa" />}
                <span style={{ fontSize: 12, color: "#9ca3af" }}>
                  {t("list.groupCount", { count: items.length, connected })}
                </span>
              </div>
              {!isCollapsed && (
                <table style={{ width: "100%", borderCollapse: "collapse", marginTop: 4 }}>
                  {tableHeader}
                  <tbody>
                    {items.map(renderRow)}
                  </tbody>
                </table>
              )}
            </div>
          );
        })
      )}
    </div>
  );
}

function Btn({
  onClick, children, color, disabled, title,
}: {
  onClick: () => void; children: React.ReactNode; color: string;
  disabled?: boolean; title?: string;
}) {
  return (
    <button
      onClick={disabled ? undefined : onClick}
      disabled={disabled}
      title={title}
      style={{
        marginRight: 4, padding: "3px 8px", fontSize: 12,
        backgroundColor: "transparent",
        border: `1px solid ${disabled ? "#374151" : color}`,
        color: disabled ? "#4b5563" : color, borderRadius: 4,
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.5 : 1,
      }}
      onMouseEnter={(e) => { if (!disabled) (e.target as HTMLButtonElement).style.backgroundColor = color + "22"; }}
      onMouseLeave={(e) => { if (!disabled) (e.target as HTMLButtonElement).style.backgroundColor = "transparent"; }}
    >
      {children}
    </button>
  );
}

const th: React.CSSProperties = { textAlign: "left", padding: "8px 12px", fontWeight: 500 };
const td: React.CSSProperties = { padding: "10px 12px", verticalAlign: "top" };
