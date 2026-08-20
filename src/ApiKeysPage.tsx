import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Activity, ArrowLeft, BarChart3, Check, Copy, Download, Eye, EyeOff, Gauge, KeyRound,
  Pencil, Plus, RefreshCw, Trash2, Wallet, Zap,
} from "lucide-react";
import { command, isTauri } from "./api";
import { CandleLineChart } from "./CandleLineChart";
import { emptyUsage, fetchers, queryKeys } from "./queries";
import type { LocalApiKey, LocalApiKeyWithToken } from "./types";

type Props = {
  localKeys: LocalApiKey[];
  refresh: () => Promise<void>;
  success: (text: string) => void;
  fail: (error: unknown) => void;
};

export function ApiKeysPage({ localKeys, refresh, success, fail }: Props) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<LocalApiKeyWithToken | null>(null);
  const [period, setPeriod] = useState<"24h" | "7d" | "30d" | "all">("7d");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const usageQuery = useQuery({
    queryKey: queryKeys.usage(period),
    queryFn: () => fetchers.usage(period),
    enabled: isTauri() && !selectedId,
    refetchInterval: 10_000,
  });
  const detailQuery = useQuery({
    queryKey: queryKeys.keyUsage(selectedId ?? "", period),
    queryFn: () => fetchers.keyUsage(selectedId ?? "", period),
    enabled: isTauri() && !!selectedId,
    refetchInterval: 10_000,
  });
  const usage = usageQuery.data ?? emptyUsage;
  const detail = detailQuery.data ?? null;
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
    const successRate = detail?.request_count ? `${Math.round(detail.success_count / detail.request_count * 100)}%` : "—";
    return <>
      <button className="text-button keys-back" onClick={() => { setSelectedId(null); setToken(null); }}><ArrowLeft size={15} />Back to API keys</button>
      <PageHead eyebrow="API key" title={key?.name ?? detail?.name ?? "API key"} description={key?.revoked_at ? "This key is revoked and can no longer authenticate requests." : "Reveal, rotate or revoke this client token and inspect usage by model."} action={periodPicker} />
      {key && !key.revoked_at && <section className="panel key-secret-panel">
        <div className="secret"><code>{token ?? "••••••••••••••••••••••••"}</code><button className="icon-button" title={token ? "Hide" : "Reveal"} onClick={() => void reveal()}>{token ? <EyeOff size={15} /> : <Eye size={15} />}</button>{token && <CopyButton value={token} />}</div>
        <div className="api-key-actions"><button className="icon-button" title="Rename" onClick={() => void rename()}><Pencil size={15} /></button><button className="secondary" onClick={() => void rotate()}><RefreshCw size={15} />Rotate</button><button className="icon-button danger" title="Revoke" onClick={() => void revoke()}><Trash2 size={15} /></button></div>
      </section>}
      <div className="metric-grid usage-metrics">
        <Metric icon={<Activity />} value={formatNumber(detail?.request_count ?? 0)} label="Requests" />
        <Metric icon={<Check />} value={successRate} label="Success rate" />
        <Metric icon={<Gauge />} value={detail ? `${Math.round(detail.average_latency_ms)} ms` : "—"} label="Average latency" />
        <Metric icon={<Zap />} value={formatToks(detail?.tokens_per_second)} label="Current tokens/s" />
        <Metric icon={<Wallet />} value={formatUsd(detail?.estimated_cost_usd)} label="Theoretical cost" />
        <Metric icon={<Download />} value={formatNumber(detail?.input_tokens ?? 0)} label="Input tokens" />
        <Metric icon={<BarChart3 />} value={formatNumber(detail?.output_tokens ?? 0)} label="Output tokens" />
        <Metric icon={<KeyRound />} value={formatNumber((detail?.cache_read_tokens ?? 0) + (detail?.cache_write_tokens ?? 0))} label="Cache tokens" />
      </div>
      <section className="panel usage-chart-panel"><div className="panel-title"><div><h3>Tokens / second</h3><p><i className="legend candles" /> OHLC <i className="legend line" /> Average</p></div></div>
        <CandleLineChart candles={detail?.throughput_candles ?? []} unit="tok/s" formatValue={value => value.toFixed(1)} empty={<Empty icon={<Zap />} title="No throughput yet" text="Successful completions with output tokens will appear here." />} />
      </section>
      <section className="panel usage-chart-panel"><div className="panel-title"><div><h3>Cost over time</h3><p>List prices × tokens for requests authenticated with this key.</p></div></div>
        <CandleLineChart candles={detail?.cost_candles ?? []} unit="USD" formatValue={value => formatUsd(value)} empty={<Empty icon={<Wallet />} title="No priced usage" text="Cloud requests with known list prices will estimate spend here. Local models are $0." />} />
      </section>
      <section className="panel"><div className="panel-title"><div><h3>Usage by model</h3><p>Requested alias and the target that served the request.</p></div></div>
        <div className="usage-table"><div className="usage-head model-stats-head"><span>Alias</span><span>Target</span><span>Requests</span><span>Success</span><span>Tokens/s</span><span>Cost</span><span>Input</span><span>Output</span></div>
          {(detail?.by_model ?? []).map(item => <div className="usage-row model-key-row" key={`${item.alias ?? ""}:${item.target ?? ""}`}><strong>{item.alias ?? "—"}</strong><span>{item.target ?? "—"}</span><span>{formatNumber(item.request_count)}</span><span>{item.request_count ? `${Math.round(item.success_count / item.request_count * 100)}%` : "—"}</span><span>{formatToks(item.tokens_per_second)}</span><span>{formatUsd(item.estimated_cost_usd)}</span><span>{formatNumber(item.input_tokens)}</span><span>{formatNumber(item.output_tokens)}</span></div>)}
        </div>
        {!detail?.by_model.length && <Empty icon={<KeyRound />} title="No model usage" text="Token counts appear after this key is used for inference." />}
      </section>
    </>;
  }
  const statsFor = (id: string) => usage.by_key.find(item => item.api_key_id === id);
  const activeKeys = localKeys.filter(key => !key.revoked_at).length;
  const successRate = usage.request_count ? `${Math.round(usage.success_count / usage.request_count * 100)}%` : "—";
  const totalTokens = usage.input_tokens + usage.output_tokens;
  return <>
    <PageHead eyebrow="Access" title="API keys" description="Create named client keys for the local gateway. Tokens live in macOS Keychain and can be attributed in usage and logs." action={periodPicker} />
    <div className="metric-grid usage-metrics">
      <Metric icon={<KeyRound />} value={activeKeys} label="Active keys" />
      <Metric icon={<Activity />} value={formatNumber(usage.request_count)} label="Requests" />
      <Metric icon={<Check />} value={successRate} label="Success rate" />
      <Metric icon={<BarChart3 />} value={formatNumber(totalTokens)} label="Tokens" />
      <Metric icon={<Wallet />} value={formatUsd(usage.estimated_cost_usd)} label="Theoretical cost" />
      <Metric icon={<Download />} value={formatNumber(usage.cache_read_tokens ?? 0)} label="Cache read" />
    </div>
    <section className="panel api-keys-create">
      <div className="panel-title"><div><h3>Create a key</h3><p>Give each client its own token so usage stays attributable.</p></div></div>
      <div className="inline-form"><input value={name} onChange={event => setName(event.target.value)} placeholder="New key name" maxLength={80} /><button className="primary" disabled={!name.trim() || busy} onClick={() => void create()}><Plus size={16} />Create key</button></div>
      {created && <div className="created-token"><div><strong>{created.name}</strong><small>Copy this token now. You can reveal it again from the key details.</small></div><div className="secret"><code>{created.token}</code><CopyButton value={created.token} /></div></div>}
    </section>
    <section className="panel">
      <div className="panel-title"><div><h3>Traffic share</h3><p>Request share for the selected period. Last used is lifetime.</p></div></div>
      <div className="key-share-list">
        {localKeys.map(key => {
          const stats = statsFor(key.id);
          const share = usage.request_count ? (stats?.request_count ?? 0) / usage.request_count : 0;
          return <div className="key-share-row" key={key.id}><div className="key-share-meta"><strong>{key.name}</strong><span>{Math.round(share * 100)}% · {formatNumber(stats?.request_count ?? 0)} requests</span></div><div className="key-share-bar" aria-hidden="true"><i style={{ width: `${Math.max(share * 100, stats?.request_count ? 2 : 0)}%` }} /></div></div>;
        })}
        {!localKeys.length && <Empty icon={<KeyRound />} title="No API keys" text="Create a named key to authenticate local clients." />}
      </div>
    </section>
    <section className="panel">
      <div className="api-key-table">
        <div className="api-key-head"><span>API key</span><span>Requests</span><span>Success</span><span>Latency</span><span>Share</span><span>Last used</span><span>Created</span></div>
        {localKeys.map(key => {
          const stats = statsFor(key.id);
          const share = usage.request_count ? (stats?.request_count ?? 0) / usage.request_count : 0;
          return <button type="button" className={`api-key-row ${key.revoked_at ? "revoked" : ""}`} key={key.id} onClick={() => setSelectedId(key.id)}>
            <span className="api-key-title"><strong>{key.name}</strong><Badge tone={key.revoked_at ? "neutral" : "good"}>{key.revoked_at ? "Revoked" : "Active"}</Badge></span>
            <span>{formatNumber(stats?.request_count ?? 0)}</span>
            <span>{stats?.request_count ? `${Math.round(stats.success_count / stats.request_count * 100)}%` : "—"}</span>
            <span>{stats?.request_count ? `${Math.round(stats.average_latency_ms)} ms` : "—"}</span>
            <span>{Math.round(share * 100)}%</span>
            <span>{key.last_used_at ? new Date(key.last_used_at).toLocaleString() : "Never used"}</span>
            <span>{new Date(key.created_at).toLocaleDateString()}</span>
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
function formatToks(value?: number | null) {
  return value == null ? "—" : `${value.toFixed(1)} tok/s`;
}
function formatUsd(value?: number | null) {
  if (value == null) return "Unknown";
  if (value === 0) return "$0.00";
  if (Math.abs(value) < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(2)}`;
}