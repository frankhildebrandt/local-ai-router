import { useEffect, useState, type ReactNode } from "react";
import {
  Activity, ArrowLeft, BarChart3, Check, Copy, Download, Eye, EyeOff, Gauge, KeyRound,
  Pencil, Plus, RefreshCw, Trash2,
} from "lucide-react";
import { command, isTauri } from "./api";
import type { KeyUsageData, LocalApiKey, LocalApiKeyWithToken, UsageData } from "./types";

type Props = {
  localKeys: LocalApiKey[];
  refresh: () => Promise<void>;
  success: (text: string) => void;
  fail: (error: unknown) => void;
};

const emptyUsage: UsageData = { request_count: 0, success_count: 0, average_latency_ms: 0, input_tokens: 0, output_tokens: 0, unknown_usage_count: 0, buckets: [], by_key: [] };

export function ApiKeysPage({ localKeys, refresh, success, fail }: Props) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<LocalApiKeyWithToken | null>(null);
  const [period, setPeriod] = useState<"24h" | "7d" | "30d" | "all">("7d");
  const [usage, setUsage] = useState<UsageData>(emptyUsage);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<KeyUsageData | null>(null);
  const [token, setToken] = useState<string | null>(null);
  useEffect(() => {
    if (!isTauri() || selectedId) return;
    let active = true;
    const load = async () => { try { const data = await command<UsageData>("get_usage", { period }); if (active) setUsage(data); } catch (error) { fail(error); } };
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    return () => { active = false; clearInterval(timer); };
  }, [period, fail, selectedId]);
  useEffect(() => {
    if (!isTauri() || !selectedId) { setDetail(null); return; }
    let active = true;
    const load = async () => { try { const data = await command<KeyUsageData>("get_key_usage", { id: selectedId, period }); if (active) setDetail(data); } catch (error) { fail(error); } };
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    return () => { active = false; clearInterval(timer); };
  }, [selectedId, period, fail]);
  const create = async () => {
    if (!name.trim()) return;
    setBusy(true);
    try {
      const result = await command<LocalApiKeyWithToken>("create_local_api_key", { name });
      setCreated(result);
      setName("");
      await refresh();
      success("Local API key created");
    } catch (error) { fail(error); }
    finally { setBusy(false); }
  };
  const selected = localKeys.find(key => key.id === selectedId) ?? null;
  const periodPicker = <div className="segmented small period-picker">{(["24h", "7d", "30d", "all"] as const).map(value => <button key={value} className={period === value ? "selected" : ""} onClick={() => setPeriod(value)}>{value === "all" ? "All" : value}</button>)}</div>;
  if (selectedId) {
    const key = selected;
    const reveal = async () => {
      if (!key) return;
      if (token) { setToken(null); return; }
      try { setToken(await command<string>("reveal_local_api_key", { id: key.id })); } catch (error) { fail(error); }
    };
    const rename = async () => {
      if (!key) return;
      const next = prompt("API key name", key.name)?.trim();
      if (!next || next === key.name) return;
      try { await command("rename_local_api_key", { id: key.id, name: next }); await refresh(); success("API key renamed"); } catch (error) { fail(error); }
    };
    const rotate = async () => {
      if (!key) return;
      if (!confirm(`Rotate “${key.name}”? The current token will stop working immediately.`)) return;
      try { setToken(await command<string>("rotate_local_api_key", { id: key.id })); await refresh(); success("API key rotated"); } catch (error) { fail(error); }
    };
    const revoke = async () => {
      if (!key) return;
      if (!confirm(`Revoke “${key.name}”? This cannot be undone.`)) return;
      try { await command("revoke_local_api_key", { id: key.id }); setToken(null); setSelectedId(null); await refresh(); success("API key revoked"); } catch (error) { fail(error); }
    };
    const maxRequests = Math.max(1, ...(detail?.buckets ?? []).map(bucket => bucket.request_count));
    const maxTokens = Math.max(1, ...(detail?.buckets ?? []).map(bucket => bucket.input_tokens + bucket.output_tokens));
    return <>
      <button className="text-button keys-back" onClick={() => { setSelectedId(null); setToken(null); }}><ArrowLeft size={15} />Back to API keys</button>
      <PageHead eyebrow="API key" title={key?.name ?? detail?.name ?? "API key"} description={key?.revoked_at ? "This key is revoked and can no longer authenticate requests." : "Reveal, rotate or revoke this client token and inspect usage by model."} action={periodPicker} />
      {key && !key.revoked_at && <section className="panel key-secret-panel">
        <div className="secret"><code>{token ?? "••••••••••••••••••••••••"}</code><button className="icon-button" title={token ? "Hide" : "Reveal"} onClick={() => void reveal()}>{token ? <EyeOff size={15} /> : <Eye size={15} />}</button>{token && <CopyButton value={token} />}</div>
        <div className="api-key-actions"><button className="icon-button" title="Rename" onClick={() => void rename()}><Pencil size={15} /></button><button className="secondary" onClick={() => void rotate()}><RefreshCw size={15} />Rotate</button><button className="icon-button danger" title="Revoke" onClick={() => void revoke()}><Trash2 size={15} /></button></div>
      </section>}
      <div className="metric-grid">
        <Metric icon={<Activity />} value={formatNumber(detail?.request_count ?? 0)} label="Requests" />
        <Metric icon={<Download />} value={formatNumber(detail?.input_tokens ?? 0)} label="Input tokens" />
        <Metric icon={<BarChart3 />} value={formatNumber(detail?.output_tokens ?? 0)} label="Output tokens" />
        <Metric icon={<Gauge />} value={detail ? `${Math.round(detail.average_latency_ms)} ms` : "—"} label="Average latency" />
      </div>
      <section className="panel usage-chart-panel"><div className="panel-title"><div><h3>Usage over time</h3><p><i className="legend requests" /> Requests <i className="legend tokens" /> Tokens</p></div></div>
        {detail?.buckets.length ? <div className="usage-chart">{detail.buckets.map(bucket => <div className="usage-column" key={bucket.start} title={`${new Date(bucket.start).toLocaleString()} · ${bucket.request_count} requests · ${formatNumber(bucket.input_tokens + bucket.output_tokens)} tokens`}><div className="bars"><i className="request-bar" style={{ height: `${Math.max(3, bucket.request_count / maxRequests * 100)}%` }} /><i className="token-bar" style={{ height: `${Math.max(3, (bucket.input_tokens + bucket.output_tokens) / maxTokens * 100)}%` }} /></div><span>{new Date(bucket.start).toLocaleDateString(undefined, { month: "short", day: "numeric", ...(period === "24h" ? { hour: "2-digit" } : {}) })}</span></div>)}</div> : <Empty icon={<BarChart3 />} title="No usage yet" text="Authenticated inference with this key will appear here." />}
      </section>
      <section className="panel"><div className="panel-title"><div><h3>Usage by model</h3><p>Requested alias and the target that served the request.</p></div></div>
        <div className="usage-table"><div className="usage-head model-usage-head"><span>Alias</span><span>Target</span><span>Requests</span><span>Success</span><span>Latency</span><span>Input</span><span>Output</span></div>
          {(detail?.by_model ?? []).map(item => <div className="usage-row model-usage-row" key={`${item.alias ?? ""}:${item.target ?? ""}`}><strong>{item.alias ?? "—"}</strong><span>{item.target ?? "—"}</span><span>{formatNumber(item.request_count)}</span><span>{item.request_count ? `${Math.round(item.success_count / item.request_count * 100)}%` : "—"}</span><span>{Math.round(item.average_latency_ms)} ms</span><span>{formatNumber(item.input_tokens)}</span><span>{formatNumber(item.output_tokens)}</span></div>)}
        </div>
        {!detail?.by_model.length && <Empty icon={<KeyRound />} title="No model usage" text="Token counts appear after this key is used for inference." />}
      </section>
    </>;
  }
  const statsFor = (id: string) => usage.by_key.find(item => item.api_key_id === id);
  return <>
    <PageHead eyebrow="Access" title="API keys" description="Create named client keys for the local gateway. Tokens live in macOS Keychain and can be attributed in usage and logs." action={periodPicker} />
    <section className="panel api-keys-create">
      <div className="panel-title"><div><h3>Create a key</h3><p>Give each client its own token so usage stays attributable.</p></div></div>
      <div className="inline-form"><input value={name} onChange={event => setName(event.target.value)} placeholder="New key name" maxLength={80} /><button className="primary" disabled={!name.trim() || busy} onClick={() => void create()}><Plus size={16} />Create key</button></div>
      {created && <div className="created-token"><div><strong>{created.name}</strong><small>Copy this token now. You can reveal it again from the key details.</small></div><div className="secret"><code>{created.token}</code><CopyButton value={created.token} /></div></div>}
    </section>
    <section className="panel">
      <div className="api-key-table">
        <div className="api-key-head"><span>API key</span><span>Requests</span><span>Input</span><span>Output</span><span>Last used</span></div>
        {localKeys.map(key => {
          const stats = statsFor(key.id);
          return <button type="button" className={`api-key-row ${key.revoked_at ? "revoked" : ""}`} key={key.id} onClick={() => setSelectedId(key.id)}>
            <span className="api-key-title"><strong>{key.name}</strong><Badge tone={key.revoked_at ? "neutral" : "good"}>{key.revoked_at ? "Revoked" : "Active"}</Badge></span>
            <span>{formatNumber(stats?.request_count ?? 0)}</span>
            <span>{formatNumber(stats?.input_tokens ?? 0)}</span>
            <span>{formatNumber(stats?.output_tokens ?? 0)}</span>
            <span>{key.last_used_at ? new Date(key.last_used_at).toLocaleString() : "Never used"}</span>
          </button>;
        })}
      </div>
      {!localKeys.length && <Empty icon={<KeyRound />} title="No API keys" text="Create a named key to authenticate local clients." />}
    </section>
  </>;
}

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: ReactNode }) {
  return <div className="page-head"><div><span className="eyebrow">{eyebrow}</span><h1>{title}</h1><p>{description}</p></div>{action}</div>;
}
function Metric({ icon, value, label }: { icon: ReactNode; value: ReactNode; label: string }) {
  return <div className="metric"><span>{icon}</span><div><strong>{value}</strong><small>{label}</small></div></div>;
}
function Badge({ children, tone }: { children: ReactNode; tone: "good" | "warn" | "bad" | "neutral" }) {
  return <span className={`badge ${tone}`}>{children}</span>;
}
function Empty({ icon, title, text }: { icon: ReactNode; title: string; text: string }) {
  return <div className="empty"><div>{icon}</div><h3>{title}</h3><p>{text}</p></div>;
}
function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return <button className="icon-button" title="Copy" onClick={() => { void navigator.clipboard.writeText(value); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>{copied ? <Check size={16} /> : <Copy size={16} />}</button>;
}
function formatNumber(value: number) {
  return new Intl.NumberFormat().format(value);
}
