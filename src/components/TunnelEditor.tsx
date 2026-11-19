import { useState, useEffect, useRef } from "react";
import { addTunnel, updateTunnel, parseCommand } from "../api";
import type { TunnelConfig, TunnelInfo } from "../types";
import type { Notify } from "../App";
import {
  EMPTY_FORM_STATE,
  configToFormState,
  formToCommand,
  stripHostBrackets,
  validateFormState,
  type EditorFormState,
  type ForwardRow,
  type FormFieldError,
} from "./tunnelEditorForm";

interface Props {
  editTarget: TunnelInfo | null; // null = add mode
  tunnels: TunnelInfo[];
  notify: Notify;
  onClose: () => void;
  onSaved: () => void;
}

const TAB_STORAGE_KEY = "sshmgmt.editor.tab";

// Read the remembered editor tab; anything invalid or unavailable falls
// back to "form" (first-time users must land on the form tab).
function readStoredTab(): "form" | "command" {
  try {
    return window.localStorage.getItem(TAB_STORAGE_KEY) === "command"
      ? "command"
      : "form";
  } catch {
    return "form";
  }
}

// Persist the tab choice; failures only lose the preference across
// sessions, so they are silently ignored.
function writeStoredTab(tab: "form" | "command"): void {
  try {
    window.localStorage.setItem(TAB_STORAGE_KEY, tab);
  } catch {
    // ignore
  }
}

export default function TunnelEditor({ editTarget, tunnels, notify, onClose, onSaved }: Props) {
  const isEdit = editTarget !== null;
  const [activeTab, setActiveTab] = useState<"form" | "command">(readStoredTab);

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
  const [formState, setFormState] = useState<EditorFormState>(() =>
    isEdit ? configToFormState(editTarget.config) : EMPTY_FORM_STATE,
  );
  const [advancedOpen, setAdvancedOpen] = useState(false);

  // Per-render validation for red hints and the save button.
  const formErrors = validateFormState(formState);

  function updateForm(patch: Partial<EditorFormState>): void {
    setFormState((prev) => ({ ...prev, ...patch }));
  }

  function updateForward(i: number, patch: Partial<ForwardRow>): void {
    setFormState((prev) => ({
      ...prev,
      forwards: prev.forwards.map((f, idx) => (idx === i ? { ...f, ...patch } : f)),
    }));
  }

  function addForward(): void {
    setFormState((prev) => ({
      ...prev,
      forwards: [
        ...prev.forwards,
        { localPort: "", remoteHost: "", remotePort: "" },
      ],
    }));
  }

  function removeForward(i: number): void {
    setFormState((prev) => ({
      ...prev,
      forwards: prev.forwards.filter((_, idx) => idx !== i),
    }));
  }

  function fieldErrors(field: string): FormFieldError[] {
    return formErrors.filter((e) => e.field === field);
  }

  // Token to drop stale parse responses when the user flips tabs quickly:
  // each switch bumps the counter, responses from older switches are ignored.
  const switchSeqRef = useRef(0);

  function handleSwitchTab(tab: "form" | "command"): void {
    if (tab === activeTab) return;
    setActiveTab(tab);
    writeStoredTab(tab);
    if (tab === "form") {
      // Empty command is not a failure: keep the form as-is.
      if (!rawCommand.trim()) return;
      const seq = ++switchSeqRef.current;
      parseCommand(rawCommand)
        .then((cfg) => {
          if (seq !== switchSeqRef.current) return;
          setFormState(configToFormState(cfg));
        })
        .catch((e) => {
          if (seq !== switchSeqRef.current) return;
          notify("warn", `命令解析失败，表单保持原有内容：${e}`);
        });
    } else {
      const r = formToCommand(formState);
      if (r.ok) {
        setRawCommand(r.command);
      } else {
        notify("warn", "表单信息不完整，已保留原命令内容");
      }
    }
  }

  // Derived option lists from existing tunnels. Group options keep their
  // stored case; environments are normalized to uppercase so "qa" and "QA"
  // suggest as the same entry.
  const groupOptions = Array.from(
    new Set(tunnels.map((t) => t.config.group).filter((g): g is string => !!g))
  ).sort();

  const envOptions = Array.from(
    new Set(
      tunnels
        .map((t) => t.config.environment?.trim().toUpperCase())
        .filter((e): e is string => !!e)
    )
  ).sort();

  // Debounced parse preview (command tab only; switching back to the
  // command tab re-runs this so the preview matches the current text)
  useEffect(() => {
    if (activeTab !== "command") return;
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
  }, [rawCommand, activeTab]);

  // Shared save body; the command source differs per tab (see handleSave).
  const saveCommand = async (cmd: string): Promise<void> => {
    setSaving(true);
    setSaveError("");
    const g = group.trim() || null;
    // Normalize the environment to uppercase so "qa" and "QA" can never
    // become two separate environments (backend does the same).
    const env = environment.trim().toUpperCase() || null;
    try {
      if (isEdit) {
        const fresh = await parseCommand(cmd);
        await updateTunnel({
          ...fresh,
          id: editTarget.config.id,
          name: name || fresh.name,
          group: g,
          environment: env,
        });
      } else {
        await addTunnel(cmd, name || undefined, g, env);
      }
      onSaved();
      onClose();
    } catch (e) {
      setSaveError(String(e));
      notify("error", `保存隧道失败：${e}`);
    } finally {
      setSaving(false);
    }
  };

  const handleSave = async (): Promise<void> => {
    if (activeTab === "form") {
      // Validation already disables the button; this early return is a
      // second guard so a broken command can never reach the backend.
      const r = formToCommand(formState);
      if (!r.ok) return;
      await saveCommand(r.command);
    } else {
      if (!rawCommand.trim()) return;
      await saveCommand(rawCommand);
    }
  };

  return (
    <Overlay onClose={onClose}>
      <div style={{ width: 640, maxHeight: "90vh", overflowY: "auto" }}>
        <h2 style={{ marginTop: 0, marginBottom: 16 }}>
          {isEdit ? "编辑隧道" : "新增隧道"}
        </h2>

        <div style={{ display: "flex", gap: 8, marginBottom: 16 }}>
          <button
            onClick={() => handleSwitchTab("form")}
            style={activeTab === "form" ? tabActiveStyle : tabInactiveStyle}
          >
            表单填写
          </button>
          <button
            onClick={() => handleSwitchTab("command")}
            style={activeTab === "command" ? tabActiveStyle : tabInactiveStyle}
          >
            粘贴命令
          </button>
        </div>

        {activeTab === "form" && (
          <div>
            <label style={labelStyle}>跳板机用户名</label>
            <input
              value={formState.jumpUser}
              onChange={(e) => updateForm({ jumpUser: e.target.value })}
              placeholder="登录跳板机的用户名"
              style={inputStyle}
            />
            <ErrorHints errors={fieldErrors("jumpUser")} />

            <label style={{ ...labelStyle, marginTop: 12 }}>跳板机地址</label>
            <input
              value={formState.jumpHost}
              onChange={(e) => updateForm({ jumpHost: stripHostBrackets(e.target.value) })}
              placeholder="支持 IP、域名；IPv6 直接填写（如 ::1），无需方括号"
              style={inputStyle}
            />
            <ErrorHints errors={fieldErrors("jumpHost")} />

            <label style={{ ...labelStyle, marginTop: 12 }}>跳板机端口</label>
            <input
              value={formState.jumpPort}
              onChange={(e) => updateForm({ jumpPort: e.target.value })}
              placeholder="22"
              style={inputStyle}
            />
            <ErrorHints errors={fieldErrors("jumpPort")} />

            <label style={{ ...labelStyle, marginTop: 12 }}>转发规则（至少一条）</label>
            {formState.forwards.map((f, i) => (
              <div key={i} style={{ marginBottom: 8 }}>
                <div style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
                  <div style={{ width: 130 }}>
                    <input
                      value={f.localPort}
                      onChange={(e) => updateForward(i, { localPort: e.target.value })}
                      placeholder="本地端口"
                      style={inputStyle}
                    />
                    <ErrorHints errors={fieldErrors(`forwards[${i}].localPort`)} />
                  </div>
                  <div style={{ flex: 1 }}>
                    <input
                      value={f.remoteHost}
                      onChange={(e) => updateForward(i, { remoteHost: e.target.value })}
                      placeholder="目标主机"
                      style={inputStyle}
                    />
                    <ErrorHints errors={fieldErrors(`forwards[${i}].remoteHost`)} />
                  </div>
                  <div style={{ width: 130 }}>
                    <input
                      value={f.remotePort}
                      onChange={(e) => updateForward(i, { remotePort: e.target.value })}
                      placeholder="目标端口"
                      style={inputStyle}
                    />
                    <ErrorHints errors={fieldErrors(`forwards[${i}].remotePort`)} />
                  </div>
                  <button onClick={() => removeForward(i)} style={btnSecondary}>删除</button>
                </div>
              </div>
            ))}
            <ErrorHints errors={fieldErrors("forwards")} />
            <div style={{ marginTop: 8 }}>
              <button onClick={addForward} style={btnSecondary}>+ 添加转发规则</button>
            </div>
            <div style={{ color: "#6b7280", fontSize: 12, marginTop: 8 }}>
              目标主机是从跳板机看过去的地址（如内网 IP 或内网域名）
            </div>

            <div style={{ marginTop: 12 }}>
              <button onClick={() => setAdvancedOpen((v) => !v)} style={btnSecondary}>
                {advancedOpen ? "▾ 高级选项" : "▸ 高级选项"}
              </button>
              {advancedOpen && (
                <div style={{ marginTop: 8 }}>
                  <label style={{
                    display: "flex", gap: 8, alignItems: "center",
                    fontSize: 13, color: "#9ca3af", marginBottom: 8,
                  }}>
                    <input
                      type="checkbox"
                      checked={formState.bindAll}
                      onChange={(e) => updateForm({ bindAll: e.target.checked })}
                    />
                    监听所有网卡 (-g)
                  </label>
                  <label style={labelStyle}>私钥文件路径（选填）</label>
                  <input
                    value={formState.identityFile}
                    onChange={(e) => updateForm({ identityFile: e.target.value })}
                    placeholder="例如 ~/.ssh/id_ed25519，留空则不使用"
                    style={inputStyle}
                  />
                  <ErrorHints errors={fieldErrors("identityFile")} />
                </div>
              )}
            </div>
          </div>
        )}

        {activeTab === "command" && (
          <>
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
          </>
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
          placeholder="选择已有环境，或输入新环境（保存时统一大写）"
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
            disabled={
              saving ||
              (activeTab === "form"
                ? formErrors.length > 0
                : !rawCommand.trim() || !!parseError)
            }
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

// Per-field red hint lines under form inputs.
function ErrorHints({ errors }: { errors: FormFieldError[] }) {
  if (errors.length === 0) return null;
  return (
    <>
      {errors.map((e, i) => (
        <div key={i} style={{ color: "#f87171", fontSize: 12, marginTop: 4 }}>
          {e.message}
        </div>
      ))}
    </>
  );
}

export function Overlay({ children }: { children: React.ReactNode; onClose?: () => void }) {
  return (
    <div
      style={{
        position: "fixed", inset: 0, backgroundColor: "rgba(0,0,0,0.7)",
        display: "flex", alignItems: "center", justifyContent: "center", zIndex: 100,
      }}
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

const tabActiveStyle: React.CSSProperties = {
  padding: "6px 16px", backgroundColor: "#3b82f6", color: "#fff",
  border: "none", borderRadius: 6, cursor: "pointer", fontSize: 13,
};

const tabInactiveStyle: React.CSSProperties = {
  padding: "6px 16px", backgroundColor: "transparent", color: "#9ca3af",
  border: "1px solid #374151", borderRadius: 6, cursor: "pointer", fontSize: 13,
};
