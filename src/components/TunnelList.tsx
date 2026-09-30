import { useMemo, useState } from "react";
import type { TunnelInfo, TunnelState } from "../types";

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

const FILTER_LABEL: Record<StateFilter, string> = {
  all: "全部",
  connected: "已连接",
  connecting: "连接中",
  failed: "失败",
  disconnected: "已断开",
};

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
const UNGROUPED_LABEL = "未分组";

function StateLight({ state }: { state: TunnelState }) {
  const color = STATE_COLOR[state.type] ?? "#374151";
  const title = state.type === "Failed" ? `Failed: ${state.message}` : state.type;
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
    ...t.config.forwards.map((f) => f.remote_host),
    ...t.config.forwards.map((f) => String(f.local_port)),
    ...t.config.forwards.map((f) => String(f.remote_port)),
  ];
  return fields.some((s) => s && s.toLowerCase().includes(q));
}

export default function TunnelList({
  tunnels, groupOrder, onGroupOrderChange,
  collapsedGroups, onCollapsedChange,
  onConnect, onDisconnect, onReconnect, onEdit, onDelete,
}: Props) {
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
        <div>暂无隧道。点击「新增隧道」粘贴 ssh 命令开始。</div>
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

  const renderRow = (t: TunnelInfo) => (
    <tr key={t.config.id} style={{ borderBottom: "1px solid #1f2937" }}>
      <td style={td}>
        {t.config.group
          ? <Tag label={t.config.group} color="#a78bfa" />
          : <span style={{ color: "#4b5563", fontSize: 12 }}>—</span>}
      </td>
      <td style={td}>
        {t.config.environment
          ? <Tag label={t.config.environment.toUpperCase()} color="#60a5fa" />
          : <span style={{ color: "#4b5563", fontSize: 12 }}>—</span>}
      </td>
      <td style={td}>{t.config.name}</td>
      <td style={td}>
        {t.config.forwards.map((f) => (
          <div key={f.local_port} style={{ fontSize: 13 }}>
            <span style={{ color: "#60a5fa" }}>:{f.local_port}</span>
          </div>
        ))}
      </td>
      <td style={td}>
        {t.config.forwards.map((f) => (
          <div key={f.local_port} style={{ fontSize: 12, color: "#d1d5db" }}>
            {f.remote_host}:{f.remote_port}
          </div>
        ))}
      </td>
      <td style={td}>
        <span style={{ fontSize: 12 }}>
          {t.config.jump_user}@{t.config.jump_host}
          {t.config.jump_port !== 22 ? `:${t.config.jump_port}` : ""}
        </span>
      </td>
      <td style={td}>
        <StateLight state={t.state} />
      </td>
      <td style={{ ...td, whiteSpace: "nowrap" }}>
        {!isActive(t) ? (
          <Btn onClick={() => onConnect(t.config.id)} color="#3b82f6">连接</Btn>
        ) : (
          <Btn onClick={() => onDisconnect(t.config.id)} color="#6b7280">断开</Btn>
        )}
        <Btn onClick={() => onReconnect(t.config.id)} color="#f59e0b">重连</Btn>
        <Btn onClick={() => onEdit(t)} color="#8b5cf6">编辑</Btn>
        {confirmId === t.config.id ? (
          <>
            <Btn onClick={() => { onDelete(t.config.id); setConfirmId(null); }} color="#dc2626">确认删除</Btn>
            <Btn onClick={() => setConfirmId(null)} color="#6b7280">取消</Btn>
          </>
        ) : (
          <Btn onClick={() => setConfirmId(t.config.id)} color="#ef4444">删除</Btn>
        )}
      </td>
    </tr>
  );

  const tableHeader = (
    <thead>
      <tr style={{ borderBottom: "1px solid #374151", color: "#9ca3af", fontSize: 12 }}>
        <th style={th}>项目</th>
        <th style={th}>环境</th>
        <th style={th}>名称</th>
        <th style={th}>监听端口</th>
        <th style={th}>目标</th>
        <th style={th}>跳板机</th>
        <th style={th}>状态</th>
        <th style={th}>操作</th>
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
          placeholder="搜索名称 / 项目 / 环境 / 主机 / 端口…"
          style={{
            flex: 1, minWidth: 220, maxWidth: 420, padding: "7px 12px",
            fontSize: 13, color: "#f9fafb", backgroundColor: "#1f2937",
            border: "1px solid #374151", borderRadius: 6, outline: "none",
          }}
        />
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {(Object.keys(FILTER_LABEL) as StateFilter[]).map((f) => (
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
              {FILTER_LABEL[f]}({counts[f]})
            </button>
          ))}
          <button
            onClick={anyExpanded ? collapseAll : expandAll}
            title={anyExpanded ? "收起所有分组" : "展开所有分组"}
            style={{
              padding: "4px 10px", fontSize: 12, borderRadius: 12,
              cursor: "pointer", color: "#9ca3af", backgroundColor: "transparent",
              border: "1px solid #374151",
            }}
          >
            {anyExpanded ? "收起全部" : "展开全部"}
          </button>
        </div>
      </div>

      {filtered.length === 0 ? (
        <div style={{ textAlign: "center", padding: "40px 0", color: "#6b7280" }}>
          没有匹配的隧道
        </div>
      ) : (
        Array.from(groups.entries()).map(([key, items]) => {
          const isUngrouped = key === UNGROUPED_KEY;
          const label = isUngrouped ? UNGROUPED_LABEL : key;
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
                  title={canDrag ? "拖动调整分组顺序" : "搜索/筛选时不可拖动"}
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
                  {items.length} 条 · 已连接 {connected}
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
