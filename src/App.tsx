import { useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import {
  Activity, BookOpen, Box, Check, ChevronRight, CircleAlert, Cloud, Copy, Database,
  Download, FileDown, Gauge, KeyRound, Layers3, ListRestart, LoaderCircle, Menu,
  Play, Plus, RefreshCw, Route, Search, Server, Settings, ShieldCheck, Square,
  Trash2, X,
} from "lucide-react";
import { command, errorMessage, isTauri } from "./api";
import type { DashboardData, ModelRoute, ModelTarget, Provider, RequestLog, TargetKind } from "./types";

type Page = "overview" | "providers" | "cloud" | "local" | "routes" | "logs" | "settings";

const nav: Array<{ page: Page; label: string; icon: typeof Activity }> = [
  { page: "overview", label: "Overview", icon: Gauge },
  { page: "providers", label: "Providers", icon: KeyRound },
  { page: "cloud", label: "Cloud models", icon: Cloud },
  { page: "local", label: "Local models", icon: Box },
  { page: "routes", label: "Aliases & routes", icon: Route },
  { page: "logs", label: "Request logs", icon: Activity },
  { page: "settings", label: "Settings", icon: Settings },
];

const emptyDashboard: DashboardData = { running: false, base_url: "http://127.0.0.1:11435/v1", provider_count: 0, target_count: 0, route_count: 0, recent_requests: 0, runtimes: [] };

export default function App() {
  const [page, setPage] = useState<Page>("overview");
  const [dashboard, setDashboard] = useState(emptyDashboard);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [targets, setTargets] = useState<ModelTarget[]>([]);
  const [routes, setRoutes] = useState<ModelRoute[]>([]);
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [settings, setSettings] = useState<Record<string, string>>({});
  const [notice, setNotice] = useState<{ type: "error" | "success"; text: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [sidebar, setSidebar] = useState(true);

  const refresh = useCallback(async () => {
    if (!isTauri()) { setLoading(false); return; }
    try {
      const [dash, p, t, r, l, s] = await Promise.all([
        command<DashboardData>("dashboard"), command<Provider[]>("list_providers"), command<ModelTarget[]>("list_targets"),
        command<ModelRoute[]>("list_routes"), command<RequestLog[]>("list_logs", { limit: 250 }), command<Record<string, string>>("get_settings"),
      ]);
      setDashboard(dash); setProviders(p); setTargets(t); setRoutes(r); setLogs(l); setSettings(s);
    } catch (error) { setNotice({ type: "error", text: errorMessage(error) }); }
    finally { setLoading(false); }
  }, []);

  useEffect(() => { void refresh(); const timer = window.setInterval(() => { if (page === "overview" || page === "logs") void refresh(); }, 10_000); return () => clearInterval(timer); }, [refresh, page]);

  const success = (text: string) => { setNotice({ type: "success", text }); window.setTimeout(() => setNotice(null), 3500); };
  const fail = (error: unknown) => setNotice({ type: "error", text: errorMessage(error) });
  const common = { providers, targets, routes, logs, settings, refresh, success, fail };

  return <div className="shell">
    <aside className={sidebar ? "sidebar" : "sidebar collapsed"}>
      <div className="brand"><div className="brand-mark"><Layers3 size={20} /></div>{sidebar && <div><strong>Local AI Router</strong><span>Private model gateway</span></div>}</div>
      <nav>{nav.map(({ page: item, label, icon: Icon }) => <button key={item} className={page === item ? "active" : ""} onClick={() => setPage(item)} title={label}><Icon size={18} />{sidebar && <span>{label}</span>}</button>)}</nav>
      <div className="sidebar-foot">
        <div className={`server-pill ${dashboard.running ? "online" : ""}`}><i />{sidebar && <span>{dashboard.running ? "Gateway online" : "Gateway offline"}</span>}</div>
        {sidebar && <small>v0.1.0 · localhost only</small>}
      </div>
    </aside>
    <main>
      <header className="topbar"><button className="icon-button" onClick={() => setSidebar(!sidebar)}><Menu size={19} /></button><div className="crumb"><span>Local AI Router</span><ChevronRight size={14} /><strong>{nav.find(item => item.page === page)?.label}</strong></div><button className="icon-button" onClick={() => void refresh()}><RefreshCw size={17} /></button></header>
      {notice && <div className={`toast ${notice.type}`}>{notice.type === "success" ? <Check size={17} /> : <CircleAlert size={17} />}<span>{notice.text}</span><button onClick={() => setNotice(null)}><X size={15} /></button></div>}
      <div className="content">
        {loading ? <Loading /> : page === "overview" ? <Overview dashboard={dashboard} targets={targets} routes={routes} onNavigate={setPage} />
          : page === "providers" ? <ProvidersPage {...common} />
          : page === "cloud" ? <CloudPage {...common} />
          : page === "local" ? <LocalPage {...common} />
          : page === "routes" ? <RoutesPage {...common} />
          : page === "logs" ? <LogsPage {...common} />
          : <SettingsPage {...common} dashboard={dashboard} />}
      </div>
    </main>
  </div>;
}

type Common = { providers: Provider[]; targets: ModelTarget[]; routes: ModelRoute[]; logs: RequestLog[]; settings: Record<string, string>; refresh: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void };

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: ReactNode }) {
  return <div className="page-head"><div><span className="eyebrow">{eyebrow}</span><h1>{title}</h1><p>{description}</p></div>{action}</div>;
}

function Overview({ dashboard, targets, routes, onNavigate }: { dashboard: DashboardData; targets: ModelTarget[]; routes: ModelRoute[]; onNavigate: (page: Page) => void }) {
  const local = targets.filter(target => target.kind === "gguf" || target.kind === "mlx");
  const snippet = `from openai import OpenAI\n\nclient = OpenAI(\n    base_url="${dashboard.base_url}",\n    api_key="YOUR_LOCAL_KEY"\n)\n\nresponse = client.chat.completions.create(\n    model="${routes[0]?.alias ?? "my-assistant"}",\n    messages=[{"role": "user", "content": "Hello!"}]\n)`;
  return <>
    <PageHead eyebrow="System" title="Your models, one local endpoint." description="Route cloud and on-device inference through a private OpenAI-compatible gateway." action={<button className="primary" onClick={() => onNavigate("routes")}><Plus size={17} />Create alias</button>} />
    <section className="status-hero"><div><div className="live-dot"><i />Live on localhost</div><h2>{dashboard.base_url}</h2><p>Bearer authentication required · prompts are never logged</p></div><CopyButton value={dashboard.base_url} label="Copy URL" /></section>
    <div className="metric-grid">
      <Metric icon={<KeyRound />} value={dashboard.provider_count} label="Providers" />
      <Metric icon={<Database />} value={dashboard.target_count} label="Model targets" />
      <Metric icon={<Route />} value={dashboard.route_count} label="Active aliases" />
      <Metric icon={<Activity />} value={dashboard.recent_requests} label="Recent requests" />
    </div>
    <div className="two-col">
      <section className="panel"><div className="panel-title"><div><h3>Quickstart</h3><p>Works with the official OpenAI SDK.</p></div><CopyButton value={snippet} /></div><pre><code>{snippet}</code></pre></section>
      <section className="panel"><div className="panel-title"><div><h3>Local runtimes</h3><p>{local.length} installed · {dashboard.runtimes.length} loaded</p></div><button className="text-button" onClick={() => onNavigate("local")}>Manage <ChevronRight size={15} /></button></div>
        <div className="stack-list">{local.length ? local.slice(0, 4).map(model => <ModelRow key={model.id} model={model} compact />) : <Empty icon={<Box />} title="No local models" text="Import an MLX folder or GGUF file." />}</div>
      </section>
    </div>
  </>;
}

function ProvidersPage({ providers, refresh, success, fail }: Common) {
  const [editing, setEditing] = useState<Provider | null | undefined>();
  const remove = async (id: string) => { if (!confirm("Delete this provider and its cloud targets?")) return; try { await command("delete_provider", { id }); await refresh(); success("Provider removed"); } catch (e) { fail(e); } };
  return <>
    <PageHead eyebrow="Credentials" title="Providers" description="Keys stay in macOS Keychain and are never written to the database." action={<button className="primary" onClick={() => setEditing(null)}><Plus size={17} />Add provider</button>} />
    <div className="cards">{providers.map(provider => <article className="provider-card" key={provider.id}><div className={`provider-logo ${provider.kind}`}><Cloud /></div><div className="grow"><div className="row"><h3>{provider.name}</h3><Badge tone={provider.enabled && provider.has_credential ? "good" : "warn"}>{provider.enabled && provider.has_credential ? "Connected" : "Needs attention"}</Badge></div><p>{provider.base_url}</p><small>{provider.kind === "open_ai" ? "OpenAI" : "OpenRouter"} · Credential {provider.has_credential ? "stored in Keychain" : "missing"}</small></div><button className="icon-button" onClick={() => setEditing(provider)}><Settings size={17} /></button><button className="icon-button danger" onClick={() => void remove(provider.id)}><Trash2 size={17} /></button></article>)}</div>
    {!providers.length && <Empty icon={<KeyRound />} title="Connect your first provider" text="Add an OpenAI or OpenRouter key to discover available models." action={<button className="primary" onClick={() => setEditing(null)}><Plus size={17} />Add provider</button>} />}
    {editing !== undefined && <ProviderModal provider={editing} close={() => setEditing(undefined)} done={async () => { setEditing(undefined); await refresh(); success("Provider verified and saved"); }} fail={fail} />}
  </>;
}

function ProviderModal({ provider, close, done, fail }: { provider: Provider | null; close: () => void; done: () => Promise<void>; fail: (e: unknown) => void }) {
  const [kind, setKind] = useState<TargetKind>(provider?.kind ?? "open_ai"); const [name, setName] = useState(provider?.name ?? "OpenAI"); const [url, setUrl] = useState(provider?.base_url ?? "https://api.openai.com/v1"); const [key, setKey] = useState(""); const [busy, setBusy] = useState(false);
  const choose = (next: TargetKind) => { setKind(next); if (!provider) { setName(next === "open_ai" ? "OpenAI" : "OpenRouter"); setUrl(next === "open_ai" ? "https://api.openai.com/v1" : "https://openrouter.ai/api/v1"); } };
  const submit = async (event: FormEvent) => { event.preventDefault(); setBusy(true); try { await command("save_provider", { input: { id: provider?.id ?? null, name, kind, baseUrl: url, enabled: true, apiKey: key || null } }); await done(); } catch (e) { fail(e); } finally { setBusy(false); } };
  return <Modal title={provider ? "Edit provider" : "Add provider"} close={close}><form onSubmit={submit} className="form"><div className="segmented"><button type="button" className={kind === "open_ai" ? "selected" : ""} onClick={() => choose("open_ai")}>OpenAI</button><button type="button" className={kind === "open_router" ? "selected" : ""} onClick={() => choose("open_router")}>OpenRouter</button></div><Field label="Display name"><input value={name} onChange={e => setName(e.target.value)} required /></Field><Field label="Base URL"><input value={url} onChange={e => setUrl(e.target.value)} required /></Field><Field label={provider?.has_credential ? "Replace API key (optional)" : "API key"}><input type="password" value={key} onChange={e => setKey(e.target.value)} required={!provider?.has_credential} placeholder="sk-••••••••" /></Field><div className="security-note"><ShieldCheck size={17} /><span>The key is validated with the provider before it is saved to Keychain.</span></div><ModalActions close={close} busy={busy} label="Verify & save" /></form></Modal>;
}

function CloudPage({ providers, targets, refresh, success, fail }: Common) {
  const cloud = targets.filter(target => target.kind === "open_ai" || target.kind === "open_router");
  const [providerId, setProviderId] = useState(providers[0]?.id ?? ""); const [models, setModels] = useState<string[]>([]); const [model, setModel] = useState(""); const [busy, setBusy] = useState(false);
  useEffect(() => { if (providerId) void command<string[]>("cached_provider_models", { id: providerId }).then(setModels).catch(() => setModels([])); }, [providerId]);
  const sync = async () => { if (!providerId) return; setBusy(true); try { const found = await command<string[]>("sync_provider_models", { id: providerId }); setModels(found); success(`${found.length} models discovered`); } catch (e) { fail(e); } finally { setBusy(false); } };
  const add = async () => { const provider = providers.find(item => item.id === providerId); if (!provider || !model) return; try { await command("save_target", { target: { id: crypto.randomUUID(), provider_id: provider.id, name: model, kind: provider.kind, provider_model: model, local_path: null, runtime_url: null, capabilities: ["chat", "embeddings", "images", "audio", "moderation"], enabled: true, state: "ready", size_bytes: null } }); setModel(""); await refresh(); success("Cloud model added"); } catch (e) { fail(e); } };
  return <><PageHead eyebrow="Catalog" title="Cloud models" description="Discover provider models, then expose only the ones you choose." />
    <section className="panel toolbar-panel"><select value={providerId} onChange={e => setProviderId(e.target.value)}>{providers.map(provider => <option value={provider.id} key={provider.id}>{provider.name}</option>)}</select><button className="secondary" onClick={() => void sync()} disabled={!providerId || busy}>{busy ? <LoaderCircle className="spin" size={17} /> : <RefreshCw size={17} />}Sync catalog</button><div className="divider" /><input list="provider-models" value={model} onChange={e => setModel(e.target.value)} placeholder="Select or enter model ID" /><datalist id="provider-models">{models.map(item => <option value={item} key={item} />)}</datalist><button className="primary" disabled={!model || !providerId} onClick={() => void add()}><Plus size={17} />Add model</button></section>
    <div className="table"><div className="table-head"><span>Model</span><span>Provider</span><span>Capabilities</span><span>Status</span><span /></div>{cloud.map(target => <div className="table-row" key={target.id}><strong>{target.name}</strong><span>{providers.find(p => p.id === target.provider_id)?.name ?? "Unknown"}</span><CapabilityList items={target.capabilities} /><Badge tone="good">Ready</Badge><DeleteTarget id={target.id} refresh={refresh} success={success} fail={fail} /></div>)}</div>
    {!cloud.length && <Empty icon={<Cloud />} title="No cloud models selected" text="Sync a provider catalog or enter a model ID manually." />}
  </>;
}

function LocalPage({ targets, refresh, success, fail }: Common) {
  const local = targets.filter(target => target.kind === "gguf" || target.kind === "mlx"); const [tab, setTab] = useState<"import" | "download">("import"); const [kind, setKind] = useState<TargetKind>("gguf"); const [source, setSource] = useState(""); const [filename, setFilename] = useState(""); const [name, setName] = useState(""); const [busy, setBusy] = useState(false);
  const browse = async () => { const result = await open({ directory: kind === "mlx", multiple: false, filters: kind === "gguf" ? [{ name: "GGUF model", extensions: ["gguf"] }] : undefined }); if (typeof result === "string") { setSource(result); setName(result.split("/").pop() ?? "Local model"); } };
  const submit = async (event: FormEvent) => { event.preventDefault(); setBusy(true); try { if (tab === "import") await command("import_local_model", { input: { source, name, kind, aliasModel: name, capabilities: ["chat"] } }); else await command("download_local_model", { input: { repoId: source, filename: filename || null, name: name || source.split("/").pop(), kind, aliasModel: name || source, capabilities: ["chat"] } }); setSource(""); setName(""); setFilename(""); await refresh(); success("Local model added to the library"); } catch (e) { fail(e); } finally { setBusy(false); } };
  const toggle = async (model: ModelTarget) => { try { await command(model.state === "ready" ? "stop_local_model" : "start_local_model", { id: model.id }); await refresh(); success(model.state === "ready" ? "Model unloaded" : "Model loaded"); } catch (e) { fail(e); } };
  return <><PageHead eyebrow="On-device" title="Local models" description="Run MLX and GGUF models privately on Apple Silicon." />
    <section className="panel add-model"><div className="segmented small"><button className={tab === "import" ? "selected" : ""} onClick={() => setTab("import")}><FileDown size={15} />Import</button><button className={tab === "download" ? "selected" : ""} onClick={() => setTab("download")}><Download size={15} />Hugging Face</button></div><form onSubmit={submit}><select value={kind} onChange={e => setKind(e.target.value as TargetKind)}><option value="gguf">GGUF · llama.cpp</option><option value="mlx">MLX · Apple Silicon</option></select><div className="input-action"><input value={source} onChange={e => setSource(e.target.value)} placeholder={tab === "import" ? "Model file or folder" : "org/model-repository"} required />{tab === "import" && <button type="button" className="secondary" onClick={() => void browse()}>Browse</button>}</div>{tab === "download" && kind === "gguf" && <input value={filename} onChange={e => setFilename(e.target.value)} placeholder="quantized-model.Q4_K_M.gguf" required />}<input value={name} onChange={e => setName(e.target.value)} placeholder="Display name" required={tab === "import"} /><button className="primary" disabled={busy}>{busy ? <LoaderCircle className="spin" size={17} /> : tab === "import" ? <FileDown size={17} /> : <Download size={17} />}{tab === "import" ? "Import" : "Download"}</button></form></section>
    <div className="model-grid">{local.map(model => <article className="model-card" key={model.id}><div className="model-icon"><Box /></div><div className="grow"><div className="row"><h3>{model.name}</h3><Badge tone={model.state === "ready" ? "good" : "neutral"}>{model.state}</Badge></div><p>{model.kind.toUpperCase()} · {formatBytes(model.size_bytes)}</p><CapabilityList items={model.capabilities} /></div><button className={model.state === "ready" ? "secondary" : "primary"} onClick={() => void toggle(model)}>{model.state === "ready" ? <Square size={15} /> : <Play size={15} />}{model.state === "ready" ? "Unload" : "Load"}</button><DeleteTarget id={model.id} refresh={refresh} success={success} fail={fail} /></article>)}</div>
    {!local.length && <Empty icon={<Box />} title="Your local library is empty" text="Import a GGUF file, an MLX model folder, or download from Hugging Face." />}
  </>;
}

function RoutesPage({ targets, routes, refresh, success, fail }: Common) {
  const [editing, setEditing] = useState<ModelRoute | null | undefined>();
  const remove = async (alias: string) => { try { await command("delete_route", { alias }); await refresh(); success("Alias deleted"); } catch (e) { fail(e); } };
  return <><PageHead eyebrow="Routing" title="Aliases & fallbacks" description="Give models stable local names and define an ordered recovery path." action={<button className="primary" onClick={() => setEditing(null)} disabled={!targets.length}><Plus size={17} />Create alias</button>} />
    <div className="route-list">{routes.map(route => <article className="route-card" key={route.alias}><div className="route-main"><div className="route-icon"><Route /></div><div><div className="row"><h3>{route.alias}</h3><Badge tone={route.enabled ? "good" : "neutral"}>{route.enabled ? "Active" : "Disabled"}</Badge></div><CapabilityList items={route.capabilities} /></div></div><div className="route-flow">{route.targets.sort((a,b) => a.priority-b.priority).map((item, index) => <div key={item.id} className="route-target"><span>{index ? `Fallback ${index}` : "Primary"}</span><strong>{targets.find(target => target.id === item.id)?.name ?? item.model}</strong></div>)}</div><button className="icon-button" onClick={() => setEditing(route)}><Settings size={17} /></button><button className="icon-button danger" onClick={() => void remove(route.alias)}><Trash2 size={17} /></button></article>)}</div>
    {!routes.length && <Empty icon={<Route />} title="No aliases yet" text="Create a stable model name and attach one or more targets." />}
    {editing !== undefined && <RouteModal route={editing} targets={targets} close={() => setEditing(undefined)} done={async () => { setEditing(undefined); await refresh(); success("Alias saved"); }} fail={fail} />}
  </>;
}

function RouteModal({ route, targets, close, done, fail }: { route: ModelRoute | null; targets: ModelTarget[]; close: () => void; done: () => Promise<void>; fail: (e: unknown) => void }) {
  const [alias, setAlias] = useState(route?.alias ?? ""); const [selected, setSelected] = useState<string[]>(route?.targets.sort((a,b) => a.priority-b.priority).map(item => item.id) ?? [targets[0]?.id].filter(Boolean)); const [busy, setBusy] = useState(false);
  const availableCapabilities = useMemo(() => selected.map(id => targets.find(target => target.id === id)?.capabilities ?? []).reduce((common, capabilities, index) => index ? common.filter(item => capabilities.includes(item)) : capabilities, [] as string[]), [selected, targets]);
  const submit = async (event: FormEvent) => { event.preventDefault(); setBusy(true); try { await command("save_route", { route: { alias, enabled: true, capabilities: availableCapabilities, targets: selected.map((id, index) => { const target = targets.find(item => item.id === id)!; return { id, kind: target.kind, model: target.provider_model, priority: (index + 1) * 10, enabled: true }; }) } }); await done(); } catch (e) { fail(e); } finally { setBusy(false); } };
  return <Modal title={route ? "Edit alias" : "Create alias"} close={close}><form className="form" onSubmit={submit}><Field label="Public model name"><input value={alias} onChange={e => setAlias(e.target.value.replace(/\s+/g, "-"))} placeholder="my-assistant" required disabled={!!route} /></Field><Field label="Targets in fallback order"><div className="target-picker">{selected.map((id, index) => <div className="picker-row" key={`${id}-${index}`}><span>{index ? `Fallback ${index}` : "Primary"}</span><select value={id} onChange={e => setSelected(selected.map((item, itemIndex) => itemIndex === index ? e.target.value : item))}>{targets.map(target => <option value={target.id} key={target.id}>{target.name} · {target.kind}</option>)}</select>{selected.length > 1 && <button type="button" className="icon-button" onClick={() => setSelected(selected.filter((_, itemIndex) => itemIndex !== index))}><X size={15} /></button>}</div>)}<button type="button" className="text-button" onClick={() => setSelected([...selected, targets[0].id])}><Plus size={15} />Add fallback</button></div></Field><div><span className="field-label">Shared capabilities</span><CapabilityList items={availableCapabilities} /></div><div className="security-note"><ListRestart size={17} /><span>Fallbacks run only for network errors, timeouts, 429 and 5xx responses—never after streaming has started.</span></div><ModalActions close={close} busy={busy} label="Save alias" /></form></Modal>;
}

function LogsPage({ logs, refresh, success, fail }: Common) {
  const [query, setQuery] = useState(""); const filtered = logs.filter(log => `${log.endpoint} ${log.alias} ${log.target} ${log.status}`.toLowerCase().includes(query.toLowerCase()));
  const clear = async () => { if (!confirm("Delete all request metadata?")) return; try { await command("clear_logs"); await refresh(); success("Logs cleared"); } catch (e) { fail(e); } };
  const exportCsv = async () => { const path = await save({ defaultPath: "local-ai-router-logs.csv", filters: [{ name: "CSV", extensions: ["csv"] }] }); if (!path) return; try { await command("export_logs_csv", { path }); success("CSV exported"); } catch (e) { fail(e); } };
  return <><PageHead eyebrow="Observability" title="Request logs" description="Metadata only. Prompt and response content is never stored." action={<div className="button-row"><button className="secondary" onClick={() => void exportCsv()}><FileDown size={16} />Export CSV</button><button className="secondary danger-text" onClick={() => void clear()}><Trash2 size={16} />Clear</button></div>} />
    <div className="search"><Search size={17} /><input value={query} onChange={e => setQuery(e.target.value)} placeholder="Filter endpoint, alias, target, status…" /></div><div className="log-table"><div className="log-head"><span>Time</span><span>Endpoint</span><span>Route</span><span>Status</span><span>Latency</span><span>Attempts</span></div>{filtered.map(log => <div className="log-row" key={log.id}><span>{new Date(log.created_at).toLocaleString()}</span><code>{log.endpoint.replace("/v1/", "")}</code><span><strong>{log.alias ?? "—"}</strong><small>{log.target ?? "No target"}</small></span><Badge tone={log.status < 400 ? "good" : log.status < 500 ? "warn" : "bad"}>{log.status}</Badge><span>{log.latency_ms} ms</span><span>{log.attempts}</span></div>)}</div>{!filtered.length && <Empty icon={<Activity />} title="No matching requests" text="Requests will appear here without their content." />}
  </>;
}

function SettingsPage({ settings, dashboard, refresh, success, fail }: Common & { dashboard: DashboardData }) {
  const [token, setToken] = useState(""); const [showToken, setShowToken] = useState(false); const [autostart, setAutostart] = useState(false); const [hf, setHf] = useState("");
  useEffect(() => { if (isTauri()) { void command<string>("get_local_api_key").then(setToken).catch(fail); void isEnabled().then(setAutostart).catch(() => {}); } }, []);
  const rotate = async () => { if (!confirm("Rotate the local API key? Existing clients will stop working immediately.")) return; try { setToken(await command("rotate_local_api_key")); setShowToken(true); success("Local API key rotated"); } catch (e) { fail(e); } };
  const toggleAutostart = async () => { try { autostart ? await disable() : await enable(); setAutostart(!autostart); success(`Launch at login ${autostart ? "disabled" : "enabled"}`); } catch (e) { fail(e); } };
  const saveNumber = async (key: string, value: string) => { try { await command("save_setting", { key, value }); await refresh(); success("Setting saved; runtime changes apply after restart"); } catch (e) { fail(e); } };
  return <><PageHead eyebrow="Application" title="Settings" description="Security, resources and background behavior." />
    <div className="settings-list"><Setting title="Local endpoint" description="The gateway is bound to 127.0.0.1 and is not reachable from your network."><code>{dashboard.base_url}</code></Setting><Setting title="Local API key" description="Stored in macOS Keychain. Rotation immediately invalidates the old key."><div className="secret"><code>{showToken ? token : "••••••••••••••••••••••••"}</code><button className="icon-button" onClick={() => setShowToken(!showToken)}><KeyRound size={16} /></button><CopyButton value={token} /><button className="secondary" onClick={() => void rotate()}>Rotate</button></div></Setting><Setting title="Launch at login" description="Keep the menu bar gateway available after signing in."><Toggle checked={autostart} onChange={() => void toggleAutostart()} /></Setting><Setting title="Memory budget" description="Maximum share of physical memory reserved for resident models."><NumberSetting value={settings.memory_budget_percent ?? "70"} suffix="%" onSave={value => void saveNumber("memory_budget_percent", value)} /></Setting><Setting title="Idle unload" description="Unload an inactive local model after this period."><NumberSetting value={settings.idle_unload_minutes ?? "15"} suffix="min" onSave={value => void saveNumber("idle_unload_minutes", value)} /></Setting><Setting title="Log retention" description="Metadata older than this is removed automatically."><NumberSetting value={settings.log_retention_days ?? "30"} suffix="days" onSave={value => void saveNumber("log_retention_days", value)} /></Setting><Setting title="Hugging Face token" description="Optional for gated and private model repositories."><div className="inline-form"><input type="password" value={hf} onChange={e => setHf(e.target.value)} placeholder={settings.has_hf_token === "true" ? "Token stored in Keychain" : "hf_…"} /><button className="secondary" onClick={async () => { try { await command("save_hugging_face_token", { token: hf }); setHf(""); await refresh(); success("Hugging Face token saved"); } catch (e) { fail(e); } }}>Save</button></div></Setting></div>
  </>;
}

function ModelRow({ model, compact }: { model: ModelTarget; compact?: boolean }) { return <div className="model-row"><div className="model-icon small"><Box size={17} /></div><div className="grow"><strong>{model.name}</strong><span>{model.kind.toUpperCase()} {model.size_bytes ? `· ${formatBytes(model.size_bytes)}` : ""}</span></div><Badge tone={model.state === "ready" ? "good" : "neutral"}>{compact && model.state === "stopped" ? "Installed" : model.state}</Badge></div>; }
function Metric({ icon, value, label }: { icon: ReactNode; value: number; label: string }) { return <div className="metric"><span>{icon}</span><div><strong>{value}</strong><small>{label}</small></div></div>; }
function CapabilityList({ items }: { items: string[] }) { return <div className="capabilities">{items.slice(0, 4).map(item => <span key={item}>{item}</span>)}</div>; }
function Badge({ children, tone }: { children: ReactNode; tone: "good" | "warn" | "bad" | "neutral" }) { return <span className={`badge ${tone}`}>{children}</span>; }
function Field({ label, children }: { label: string; children: ReactNode }) { return <label className="field"><span>{label}</span>{children}</label>; }
function Setting({ title, description, children }: { title: string; description: string; children: ReactNode }) { return <section className="setting"><div><h3>{title}</h3><p>{description}</p></div><div>{children}</div></section>; }
function Empty({ icon, title, text, action }: { icon: ReactNode; title: string; text: string; action?: ReactNode }) { return <div className="empty"><div>{icon}</div><h3>{title}</h3><p>{text}</p>{action}</div>; }
function Loading() { return <div className="loading"><LoaderCircle className="spin" /><span>Loading private gateway…</span></div>; }
function Modal({ title, close, children }: { title: string; close: () => void; children: ReactNode }) { return <div className="modal-backdrop" onMouseDown={close}><div className="modal" onMouseDown={e => e.stopPropagation()}><div className="modal-head"><h2>{title}</h2><button className="icon-button" onClick={close}><X size={18} /></button></div>{children}</div></div>; }
function ModalActions({ close, busy, label }: { close: () => void; busy: boolean; label: string }) { return <div className="modal-actions"><button type="button" className="secondary" onClick={close}>Cancel</button><button className="primary" disabled={busy}>{busy && <LoaderCircle className="spin" size={16} />}{label}</button></div>; }
function CopyButton({ value, label }: { value: string; label?: string }) { const [copied, setCopied] = useState(false); return <button className={label ? "secondary" : "icon-button"} title="Copy" onClick={() => { void navigator.clipboard.writeText(value); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>{copied ? <Check size={16} /> : <Copy size={16} />}{label}</button>; }
function DeleteTarget({ id, refresh, success, fail }: Pick<Common, "refresh" | "success" | "fail"> & { id: string }) { return <button className="icon-button danger" onClick={async () => { if (!confirm("Delete this model target?")) return; try { await command("delete_target", { id }); await refresh(); success("Model target deleted"); } catch (e) { fail(e); } }}><Trash2 size={16} /></button>; }
function Toggle({ checked, onChange }: { checked: boolean; onChange: () => void }) { return <button className={`toggle ${checked ? "on" : ""}`} onClick={onChange}><i /></button>; }
function NumberSetting({ value: initial, suffix, onSave }: { value: string; suffix: string; onSave: (value: string) => void }) { const [value, setValue] = useState(initial); return <div className="number-setting"><input type="number" min="1" max="95" value={value} onChange={e => setValue(e.target.value)} onBlur={() => onSave(value)} /><span>{suffix}</span></div>; }
function formatBytes(value: number | null) { if (!value) return "Size unknown"; const units = ["B", "KB", "MB", "GB", "TB"]; const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1); return `${(value / 1024 ** index).toFixed(index > 2 ? 1 : 0)} ${units[index]}`; }
