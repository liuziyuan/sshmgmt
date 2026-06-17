import type { TunnelInfo, TunnelState } from "../types";

interface Props {
  tunnels: TunnelInfo[];
  onConnect: (id: string) => void;
  onDisconnect: (id: string) => void;
  onReconnect: (id: string) => void;
  onEdit: (info: TunnelInfo) => void;
  onDelete: (id: string) => void;
  onUploadKey: (id: string) => void;
}

const STATE_COLOR: Record<string, string> = {
  Connected:        "#22c55e",
  Connecting:       "#f59e0b",
  Reconnecting:     "#f59e0b",
  Disconnected:     "#374151",
  PasswordRequired: "#3b82f6",
  Failed:           "#ef4444",
};

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

export default function TunnelList({
  tunnels, onConnect, onDisconnect, onReconnect, onEdit, onDelete, onUploadKey,
}: Props) {
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

  // Sort: group (null last) → environment (null last) → name
  const sorted = [...tunnels].sort((a, b) => {
    const ga = a.config.group ?? "￿";
    const gb = b.config.group ?? "￿";
    if (ga !== gb) return ga.localeCompare(gb);
    const ea = a.config.environment ?? "￿";
    const eb = b.config.environment ?? "￿";
    if (ea !== eb) return ea.localeCompare(eb);
    return a.config.name.localeCompare(b.config.name);
  });

  return (
    <table style={{ width: "100%", borderCollapse: "collapse" }}>
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
      <tbody>
        {sorted.map((t) => (
          <tr key={t.config.id} style={{ borderBottom: "1px solid #1f2937" }}>
            <td style={td}>
              {t.config.group
                ? <Tag label={t.config.group} color="#a78bfa" />
                : <span style={{ color: "#4b5563", fontSize: 12 }}>—</span>}
            </td>
            <td style={td}>
              {t.config.environment
                ? <Tag label={t.config.environment} color="#60a5fa" />
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
              <Btn onClick={() => onUploadKey(t.config.id)} color="#10b981" disabled title="暂不支持，请手动打通公钥免密">上传公钥</Btn>
              <Btn onClick={() => onDelete(t.config.id)} color="#ef4444">删除</Btn>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
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
