import { useState, useEffect } from "react";
import { addTunnel, updateTunnel, parseCommand } from "../api";
import type { TunnelConfig, TunnelInfo } from "../types";

interface Props {
  editTarget: TunnelInfo | null; // null = add mode
  tunnels: TunnelInfo[];
  onClose: () => void;
  onSaved: () => void;
}

export default function TunnelEditor({ editTarget, tunnels, onClose, onSaved }: Props) {
  const isEdit = editTarget !== null;

  const [rawCommand, setRawCommand] = useState(
    isEdit ? editTarget.config.raw_command : ""
  );
  const [name, setName] = useState(isEdit ? editTarget.config.name : "");
  const [group, setGroup] = useState(isEdit ? (editTarget.config.group ?? "") : "");
  const [environment, setEnvironment] = useState(isEdit ? (editTarget.config.environment ?? "") : "");
  const [preview, setPreview] = useState<TunnelConfig | null>(null);
  const [parseError, setParseError] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");

  // Derived option lists from existing tunnels
  const groupOptions = Array.from(
    new Set(tunnels.map((t) => t.config.group).filter((g): g is string => !!g))
  ).sort();

  const envOptions = Array.from(
    new Set(
      tunnels
        .filter((t) => !!group && t.config.group === group)
        .map((t) => t.config.environment)
        .filter((e): e is string => !!e)
    )
  ).sort();

  // Debounced parse preview
  useEffect(() => {
    if (!rawCommand.trim()) {
      setPreview(null);
      setParseError("");
      return;
    }
    const t = setTimeout(async () => {
      try {
        const cfg = await parseCommand(rawCommand);
        setPreview(cfg);
        setParseError("");
        if (!name && !isEdit) setName(cfg.name);
      } catch (e) {
        setPreview(null);
        setParseError(String(e));
      }
    }, 400);
    return () => clearTimeout(t);
  }, [rawCommand]);

  const handleSave = async () => {
    if (!rawCommand.trim()) return;
    setSaving(true);
    setSaveError("");
    const g = group.trim() || null;
    const env = environment.trim() || null;
    try {
      if (isEdit) {
        const fresh = await parseCommand(rawCommand);
        await updateTunnel({
          ...fresh,
          id: editTarget.config.id,
          name: name || fresh.name,
          group: g,
          environment: env,
        });
      } else {
        await addTunnel(rawCommand, name || undefined, g, env);
      }
      onSaved();
      onClose();
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 640, maxHeight: "90vh", overflowY: "auto" }}>
        <h2 style={{ marginTop: 0, marginBottom: 16 }}>
          {isEdit ? "编辑隧道" : "新增隧道"}
        </h2>

        <label style={labelStyle}>ssh 命令</label>
        <textarea
          value={rawCommand}
          onChange={(e) => setRawCommand(e.target.value)}
          placeholder="粘贴 ssh 命令，例如：ssh -fNg -L 10008:host:3306 user@jump"
          style={{
            width: "100%", height: 80, fontFamily: "monospace", fontSize: 13,
            backgroundColor: "#1f2937", color: "#f9fafb", border: "1px solid #374151",
            borderRadius: 6, padding: 10, resize: "vertical", boxSizing: "border-box",
          }}
        />

        {parseError && (
          <div style={{ color: "#f87171", fontSize: 12, marginTop: 4 }}>{parseError}</div>
        )}

        {preview && (
          <div style={{
            backgroundColor: "#111827", border: "1px solid #374151",
            borderRadius: 6, padding: 12, marginTop: 8, fontSize: 12,
          }}>
            <div style={{ color: "#9ca3af", marginBottom: 6 }}>解析结果预览</div>
            <Row label="跳板机" value={`${preview.jump_user}@${preview.jump_host}:${preview.jump_port}`} />
            {preview.forwards.map((f, i) => (
              <Row
                key={i}
                label={`转发 ${i + 1}`}
                value={`本地 :${f.local_port} → ${f.remote_host}:${f.remote_port}`}
              />
            ))}
            {preview.bind_all && (
              <Row label="-g" value="监听所有网卡 (0.0.0.0)" />
            )}
          </div>
        )}

        <label style={{ ...labelStyle, marginTop: 12 }}>显示名称（选填）</label>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="留空则自动生成"
          style={inputStyle}
        />

        <label style={{ ...labelStyle, marginTop: 12 }}>项目 / 分组（选填）</label>
        <input
          list="group-options"
          value={group}
          onChange={(e) => { setGroup(e.target.value); setEnvironment(""); }}
          placeholder="选择或输入新组名"
          style={inputStyle}
        />
        <datalist id="group-options">
          {groupOptions.map((g) => <option key={g} value={g} />)}
        </datalist>

        <label style={{ ...labelStyle, marginTop: 12 }}>环境（选填）</label>
        <input
          list="env-options"
          value={environment}
          onChange={(e) => setEnvironment(e.target.value)}
          placeholder={group.trim() ? "选择或输入新环境（如 dev、prod）" : "先填写分组后可选已有环境"}
          style={inputStyle}
        />
        <datalist id="env-options">
          {envOptions.map((e) => <option key={e} value={e} />)}
        </datalist>

        {saveError && (
          <div style={{ color: "#f87171", fontSize: 12, marginTop: 8 }}>{saveError}</div>
        )}

        <div style={{ display: "flex", gap: 8, marginTop: 16, justifyContent: "flex-end" }}>
          <button onClick={onClose} style={btnSecondary}>取消</button>
          <button
            onClick={handleSave}
            disabled={saving || !rawCommand.trim() || !!parseError}
            style={btnPrimary}
          >
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </Overlay>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ display: "flex", gap: 8, marginBottom: 4 }}>
      <span style={{ color: "#6b7280", minWidth: 70 }}>{label}:</span>
      <span style={{ color: "#e5e7eb" }}>{value}</span>
    </div>
  );
}

export function Overlay({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div
      style={{
        position: "fixed", inset: 0, backgroundColor: "rgba(0,0,0,0.7)",
        display: "flex", alignItems: "center", justifyContent: "center", zIndex: 100,
      }}
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div style={{
        backgroundColor: "#1e2433", borderRadius: 10, padding: 24,
        border: "1px solid #374151", boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
        minWidth: 400,
      }}>
        {children}
      </div>
    </div>
  );
}

const labelStyle: React.CSSProperties = {
  display: "block", fontSize: 13, color: "#9ca3af", marginBottom: 4,
};

const inputStyle: React.CSSProperties = {
  width: "100%", padding: "8px 10px",
  backgroundColor: "#1f2937", color: "#f9fafb",
  border: "1px solid #374151", borderRadius: 6, fontSize: 13,
  boxSizing: "border-box",
};

const btnPrimary: React.CSSProperties = {
  padding: "8px 20px", backgroundColor: "#3b82f6", color: "#fff",
  border: "none", borderRadius: 6, cursor: "pointer", fontSize: 14,
};

const btnSecondary: React.CSSProperties = {
  padding: "8px 20px", backgroundColor: "transparent", color: "#9ca3af",
  border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 14,
};
