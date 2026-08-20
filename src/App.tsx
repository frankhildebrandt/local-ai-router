import { lazy, Suspense, useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { keepPreviousData, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  Activity, BarChart3, BookOpen, Bot, Box, Check, ChevronRight, CircleAlert, Cloud, Copy, Database,
  Download, Eye, FileDown, Gauge, KeyRound, Layers3, ListRestart, LoaderCircle, Menu,
  Plus, RefreshCw, Route, Search, Settings, ShieldCheck, Sparkles, Timer,
  Trash2, Wallet, X, Zap,
} from "lucide-react";
import { command, appVersion, downloadTextFile, errorMessage, isTauri, listenDesktopNavigate, listenGatewayTraffic } from "./api";
import type { DashboardData, InFlightRequest, LocalApiKey, LogQuery, ModelRoute, ModelTarget, Provider, ProviderModel, ProviderPreset, PublicModel, ResourcePolicy, ResourceProfile, RouteRole, RouteTarget, RoutingConfigExport, RoutingEvaluation, RoutingPolicy, RoutingTaskDefinition, TargetKind, TargetRoutingProfile, WireProtocol } from "./types";
import { ApiKeysPage } from "./ApiKeysPage";
import { UsersPage, LoginPage } from "./UsersPage";
import { CandleLineChart } from "./CandleLineChart";
import { LocalPage } from "./LocalPage";
import { TypeaheadSelect } from "./TypeaheadSelect";
import { createQueryClient, defaultResourcePolicy, emptyDashboard, emptyUsage, fetchers, invalidateAppQueries, queryKeys } from "./queries";
import { useDebouncedValue } from "./useDebouncedValue";

type Page = "overview" | "chat" | "keys" | "users" | "usage" | "providers" | "cloud" | "local" | "routes" | "logs" | "routing" | "settings";

const nav: Array<{ page: Page; label: string; icon: typeof Activity }> = [
  { page: "overview", label: "Overview", icon: Gauge },
  { page: "chat", label: "Chat", icon: Bot },
  { page: "keys", label: "API keys", icon: KeyRound },
  { page: "users", label: "Users", icon: ShieldCheck },
  { page: "usage", label: "Usage", icon: BarChart3 },
  { page: "providers", label: "Providers", icon: KeyRound },
  { page: "cloud", label: "Cloud models", icon: Cloud },
  { page: "local", label: "Local models", icon: Box },
  { page: "routes", label: "Custom routes", icon: Route },
  { page: "logs", label: "Request logs", icon: Activity },
  { page: "routing", label: "Routing", icon: Sparkles },
  { page: "settings", label: "Settings", icon: Settings },
];

const ChatPage = lazy(() => import("./ChatPage").then(module => ({ default: module.ChatPage })));

export default function App() {
  const [queryClient] = useState(createQueryClient);
  return <QueryClientProvider client={queryClient}><AppShell /></QueryClientProvider>;
}

function AppShell() {
  const queryClient = useQueryClient();
  const [page, setPage] = useState<Page>("overview");
  const [inflight, setInflight] = useState<InFlightRequest[]>([]);
  const [notice, setNotice] = useState<{ type: "error" | "success"; text: string } | null>(null);
  const [sidebar, setSidebar] = useState(true);
  const [version, setVersion] = useState("");

  const authQuery = useQuery({ queryKey: queryKeys.auth, queryFn: fetchers.auth });
  const signedOut = !!authQuery.data?.login_required && !authQuery.data?.authenticated;
  const dashboardQuery = useQuery({ queryKey: queryKeys.dashboard, queryFn: fetchers.dashboard, refetchInterval: page === "overview" && !signedOut ? 10_000 : false, enabled: !signedOut });
  const providersQuery = useQuery({ queryKey: queryKeys.providers, queryFn: fetchers.providers, enabled: !signedOut });
  const presetsQuery = useQuery({ queryKey: queryKeys.providerPresets, queryFn: fetchers.providerPresets, enabled: !signedOut });
  const targetsQuery = useQuery({ queryKey: queryKeys.targets, queryFn: fetchers.targets, enabled: !signedOut });
  const routesQuery = useQuery({ queryKey: queryKeys.routes, queryFn: fetchers.routes, enabled: !signedOut });
  const publicModelsQuery = useQuery({ queryKey: queryKeys.publicModels, queryFn: fetchers.publicModels, enabled: !signedOut });
  const settingsQuery = useQuery({ queryKey: queryKeys.settings, queryFn: fetchers.settings, enabled: !signedOut });
  const localKeysQuery = useQuery({ queryKey: queryKeys.localKeys, queryFn: fetchers.localKeys, enabled: !signedOut });
  const resourcePolicyQuery = useQuery({ queryKey: queryKeys.resourcePolicy, queryFn: fetchers.resourcePolicy, enabled: !signedOut });
  const routingPoliciesQuery = useQuery({ queryKey: queryKeys.routingPolicies, queryFn: fetchers.routingPolicies, enabled: !signedOut });
  const routingProfilesQuery = useQuery({ queryKey: queryKeys.routingProfiles, queryFn: fetchers.routingProfiles, enabled: !signedOut });
  const routingTasksQuery = useQuery({ queryKey: queryKeys.routingTasks, queryFn: fetchers.routingTasks, enabled: !signedOut });
  const snapshotQueries = [dashboardQuery, providersQuery, presetsQuery, targetsQuery, routesQuery, publicModelsQuery, settingsQuery, localKeysQuery, resourcePolicyQuery, routingPoliciesQuery, routingProfilesQuery, routingTasksQuery];
  const dashboard = dashboardQuery.data ?? emptyDashboard;
  const providers = providersQuery.data ?? [];
  const providerPresets = presetsQuery.data ?? [];
  const targets = targetsQuery.data ?? [];
  const routes = routesQuery.data ?? [];
  const publicModels = publicModelsQuery.data ?? [];
  const settings = settingsQuery.data ?? {};
  const localKeys = localKeysQuery.data ?? [];
  const resourcePolicy = resourcePolicyQuery.data ?? defaultResourcePolicy;
  const routingPolicies = routingPoliciesQuery.data ?? [];
  const routingProfiles = routingProfilesQuery.data ?? [];
  const routingTasks = routingTasksQuery.data ?? [];
  const loading = snapshotQueries.some(query => query.isPending);
  const snapshotError = snapshotQueries.find(query => query.error)?.error;

  const refresh = useCallback(async () => { await invalidateAppQueries(queryClient); }, [queryClient]);
  useEffect(() => { void appVersion().then(setVersion).catch(() => undefined); }, []);
  useEffect(() => { if (dashboardQuery.data?.inflight) setInflight(dashboardQuery.data.inflight); }, [dashboardQuery.data]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listenGatewayTraffic(next => {
      setInflight(next);
      void queryClient.invalidateQueries({ queryKey: queryKeys.logFacets });
      void queryClient.invalidateQueries({ queryKey: queryKeys.routingAttempts });
      void queryClient.invalidateQueries({ queryKey: ["logs"] });
    }).then(fn => { unlisten = fn; }).catch(() => undefined);
    return () => unlisten?.();
  }, [queryClient]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listenDesktopNavigate(next => {
      if (nav.some(item => item.page === next)) setPage(next as Page);
    }).then(fn => { unlisten = fn; }).catch(() => undefined);
    return () => unlisten?.();
  }, []);

  const success = useCallback((text: string) => { setNotice({ type: "success", text }); window.setTimeout(() => setNotice(null), 3500); }, []);
  const fail = useCallback((error: unknown) => setNotice({ type: "error", text: errorMessage(error) }), []);
  useEffect(() => { if (snapshotError) fail(snapshotError); }, [snapshotError, fail]);
  const stopRequest = (id: string) => { void command("cancel_inflight_request", { id }).then(() => setInflight(current => current.filter(item => item.id !== id))).catch(fail); };
  const stopAll = () => { void command("cancel_all_inflight_requests").then(() => setInflight([])).catch(fail); };
  const common = { providers, providerPresets, targets, routes, routingPolicies, routingProfiles, routingTasks, localKeys, settings, resourcePolicy, refresh, success, fail };

  if (signedOut && authQuery.data) {
    return <LoginPage auth={authQuery.data} onDone={() => { void queryClient.invalidateQueries(); }} fail={fail} />;
  }

  return <div className="shell">
    <aside className={sidebar ? "sidebar" : "sidebar collapsed"}>
      <div className="sidebar-drag" data-tauri-drag-region aria-hidden="true" />
      <div className="brand" data-tauri-drag-region><div className="brand-mark"><Layers3 size={20} /></div>{sidebar && <div><strong>Local AI Router</strong><span>Private model gateway</span></div>}</div>
      <nav>{nav.map(({ page: item, label, icon: Icon }) => <button key={item} className={page === item ? "active" : ""} onClick={() => setPage(item)} title={label}><Icon size={18} />{sidebar && <span>{label}</span>}</button>)}</nav>
      <div className="sidebar-foot">
        <div className={`server-pill ${dashboard.running ? "online" : ""}`}><i />{sidebar && <span>{dashboard.running ? "Gateway online" : "Gateway offline"}</span>}</div>
        {sidebar && <small>v{version || "…"} · {dashboard.bind_mode === "lan" || dashboard.bind_mode === "address" ? "network share" : "localhost only"}</small>}
      </div>
    </aside>
    <main>
      <header className="topbar"><button className="icon-button" onClick={() => setSidebar(!sidebar)}><Menu size={19} /></button><div className="crumb" data-tauri-drag-region><span>Local AI Router</span><ChevronRight size={14} /><strong>{nav.find(item => item.page === page)?.label}</strong></div><button className="icon-button" onClick={() => void refresh()}><RefreshCw size={17} /></button></header>
      {notice && <div className={`toast ${notice.type}`}>{notice.type === "success" ? <Check size={17} /> : <CircleAlert size={17} />}<span>{notice.text}</span><button onClick={() => setNotice(null)}><X size={15} /></button></div>}
      <div className="content">
        {loading ? <Loading /> : page === "overview" ? <Overview dashboard={dashboard} inflight={inflight} targets={targets} publicModels={publicModels} onNavigate={setPage} onStop={stopRequest} onStopAll={stopAll} />
          : page === "chat" ? <Suspense fallback={<Loading />}><ChatPage publicModels={publicModels} /></Suspense>
          : page === "keys" ? <ApiKeysPage localKeys={localKeys} refresh={refresh} success={success} fail={fail} />
          : page === "users" ? <UsersPage publicModels={publicModels} refresh={refresh} success={success} fail={fail} />
          : page === "usage" ? <UsagePage />
          : page === "providers" ? <ProvidersPage {...common} />
          : page === "cloud" ? <CloudPage {...common} />
          : page === "local" ? <LocalPage {...common} />
          : page === "routes" ? <RoutesPage {...common} publicModels={publicModels} />
          : page === "logs" ? <LogsPage {...common} />
          : page === "routing" ? <RoutingLogsPage fail={fail} />
          : <SettingsPage {...common} dashboard={dashboard} />}
      </div>
    </main>
  </div>;
}

type Common = { providers: Provider[]; providerPresets: ProviderPreset[]; targets: ModelTarget[]; routes: ModelRoute[]; routingPolicies: RoutingPolicy[]; routingProfiles: TargetRoutingProfile[]; routingTasks: RoutingTaskDefinition[]; localKeys: LocalApiKey[]; settings: Record<string, string>; resourcePolicy: ResourcePolicy; refresh: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void };

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: ReactNode }) {
  return <div className="page-head"><div><span className="eyebrow">{eyebrow}</span><h1>{title}</h1><p>{description}</p></div>{action}</div>;
}

function Overview({ dashboard, inflight, targets, publicModels, onNavigate, onStop, onStopAll }: { dashboard: DashboardData; inflight: InFlightRequest[]; targets: ModelTarget[]; publicModels: PublicModel[]; onNavigate: (page: Page) => void; onStop: (id: string) => void; onStopAll: () => void }) {
  const local = targets.filter(target => target.kind === "gguf" || target.kind === "mlx");
  const exampleModel = publicModels.find(model => model.source === "adaptive")?.id
    ?? publicModels.find(model => model.capabilities.includes("chat"))?.id
    ?? "adaptive-routing";
  const snippet = `from openai import OpenAI\n\nclient = OpenAI(\n    base_url="${dashboard.base_url}",\n    api_key="YOUR_LOCAL_KEY"  # copy from API keys\n)\n\nresponse = client.chat.completions.create(\n    model="${exampleModel}",\n    messages=[{"role": "user", "content": "Hello!"}]\n)`;
  return <>
    <PageHead eyebrow="System" title="Your models, one local endpoint." description="Route cloud and on-device inference through a private OpenAI-compatible gateway." action={<button className="primary" onClick={() => onNavigate("cloud")}><Plus size={17} />Add model</button>} />
    <section className="status-hero"><div><div className="live-dot"><i />Live on localhost</div><h2>{dashboard.base_url}</h2><p>Bearer authentication required · prompts are never logged</p></div><CopyButton value={dashboard.base_url} label="Copy URL" /></section>
    <div className="metric-grid">
      <Metric icon={<KeyRound />} value={dashboard.provider_count} label="Providers" />
      <Metric icon={<Database />} value={dashboard.target_count} label="Model targets" />
      <Metric icon={<Route />} value={dashboard.route_count} label="Custom routes" />
      <Metric icon={<Activity />} value={dashboard.recent_requests} label="Recent requests" />
    </div>
    <div className="two-col">
      <section className="panel"><div className="panel-title"><div><h3>Quickstart</h3><p>Works with the official OpenAI SDK. Copy a token from API keys.</p></div><div className="button-row"><button className="text-button" onClick={() => onNavigate("keys")}>Create a key <ChevronRight size={15} /></button><CopyButton value={snippet} /></div></div><pre><code>{snippet}</code></pre></section>
      <section className="panel"><div className="panel-title"><div><h3>Local runtimes</h3><p>{dashboard.runtimes.length} loaded{local.length ? ` · ${local.length} installed` : ""}</p></div><button className="text-button" onClick={() => onNavigate("local")}>Manage <ChevronRight size={15} /></button></div>
        <div className="stack-list">{dashboard.runtimes.length ? dashboard.runtimes.map(runtime => {
          const model = targets.find(target => target.id === runtime.target_id);
          return model ? <ModelRow key={runtime.target_id} model={model} runtime={runtime} compact /> : null;
        }) : <Empty icon={<Box />} title="No models loaded" text="Load a local model from the library to see throughput here." />}</div>
      </section>
    </div>
    <section className="panel">
      <div className="panel-title"><div><h3>Active requests</h3><p>{inflight.length ? `${inflight.length} in flight` : "Live view of gateway traffic."}</p></div><div className="button-row">{inflight.length > 0 && <button className="text-button" onClick={onStopAll}>Stop all</button>}<button className="text-button" onClick={() => onNavigate("logs")}>Logs <ChevronRight size={15} /></button></div></div>
      {inflight.length ? <div className="inflight-list">{inflight.map(request => <div className="inflight-row" key={request.id}><Timer size={15} /><div><strong>{request.alias}</strong><small>{inflightDetail(request)}</small></div><code>{request.endpoint.replace("/v1/", "")}</code><Elapsed since={request.started_at} /><button type="button" className="icon-button" aria-label="Stop request" onClick={() => onStop(request.id)}><X size={15} /></button></div>)}</div> : <Empty icon={<Timer />} title="No running requests" text="Authenticated inference shows the alias and selected model here." />}
    </section>
  </>;
}

function Elapsed({ since }: { since: string }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => { const timer = window.setInterval(() => setNow(Date.now()), 1000); return () => window.clearInterval(timer); }, []);
  const ms = Math.max(0, now - new Date(since).getTime());
  return <span>{ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`}</span>;
}

function UsagePage() {
  const [period, setPeriod] = useState<"24h" | "7d" | "30d" | "all">("7d");
  const [target, setTarget] = useState<string | null>(null);
  const usageQuery = useQuery({
    queryKey: queryKeys.usage(period, target),
    queryFn: () => fetchers.usage(period, target),
    refetchInterval: 10_000,
  });
  const usage = usageQuery.data ?? emptyUsage;
  const loading = usageQuery.isPending;
  const successRate = usage.request_count ? `${Math.round(usage.success_count / usage.request_count * 100)}%` : "—";
  const models = usage.by_model ?? [];
  return <>
    <PageHead eyebrow="Observability" title="Usage" description="Provider-reported token usage, generation speed and theoretical list-price cost, without storing request content." action={<div className="segmented small period-picker">{(["24h", "7d", "30d", "all"] as const).map(value => <button key={value} className={period === value ? "selected" : ""} onClick={() => setPeriod(value)}>{value === "all" ? "All" : value}</button>)}</div>} />
    <div className="metric-grid usage-metrics">
      <Metric icon={<Activity />} value={formatNumber(usage.request_count)} label="Requests" />
      <Metric icon={<Check />} value={successRate} label="Success rate" />
      <Metric icon={<Gauge />} value={`${Math.round(usage.average_latency_ms)} ms`} label="Average latency" />
      <Metric icon={<Zap />} value={formatToks(usage.tokens_per_second)} label="Current tokens/s" />
      <Metric icon={<Wallet />} value={formatUsd(usage.estimated_cost_usd)} label="Theoretical cost" />
      <Metric icon={<Download />} value={formatNumber(usage.input_tokens)} label="Input tokens" />
      <Metric icon={<BarChart3 />} value={formatNumber(usage.output_tokens)} label="Output tokens" />
      <Metric icon={<CircleAlert />} value={formatNumber(usage.unknown_usage_count)} label="Incomplete usage" />
    </div>
    <section className="panel usage-chart-panel"><div className="panel-title"><div><h3>Tokens / second</h3><p><i className="legend candles" /> OHLC <i className="legend line" /> Average{target ? ` · ${target}` : ""}</p></div><small>{usage.unknown_usage_count} request{usage.unknown_usage_count === 1 ? "" : "s"} without complete usage</small></div>
      {loading ? <Loading /> : <CandleLineChart candles={usage.throughput_candles ?? []} unit="tok/s" formatValue={value => value.toFixed(1)} empty={<Empty icon={<Zap />} title="No throughput yet" text="Successful completions with output tokens will appear here." />} />}
    </section>
    <section className="panel usage-chart-panel"><div className="panel-title"><div><h3>Cost over time</h3><p>List prices × tokens, including cache read/write when reported.</p></div></div>
      <CandleLineChart candles={usage.cost_candles ?? []} unit="USD" formatValue={value => formatUsd(value)} empty={<Empty icon={<Wallet />} title="No priced usage" text="Cloud requests with known list prices will estimate spend here. Local models are $0." />} />
    </section>
    <section className="panel"><div className="panel-title"><div><h3>Usage by model</h3><p>Click a row to filter the charts. Alias is the requested name; target is what served it.</p></div>{target && <button className="text-button" onClick={() => setTarget(null)}>Show all</button>}</div>
      <div className="usage-table"><div className="usage-head model-stats-head"><span>Model</span><span>Requests</span><span>Tokens/s</span><span>Cost</span><span>Input</span><span>Output</span></div>
        {models.map(item => {
          const key = item.target ?? item.alias ?? "unknown";
          const selected = target != null && item.target === target;
          return <button type="button" className={`usage-row model-stats-row ${selected ? "selected" : ""}`} key={`${item.alias ?? ""}:${item.target ?? ""}`} onClick={() => setTarget(selected ? null : item.target)}>
            <strong>{item.target ?? "—"}<small>{item.alias ?? "—"}</small></strong>
            <span>{formatNumber(item.request_count)}</span>
            <span>{formatToks(item.tokens_per_second)}</span>
            <span>{formatUsd(item.estimated_cost_usd)}</span>
            <span>{formatNumber(item.input_tokens)}</span>
            <span>{formatNumber(item.output_tokens)}</span>
          </button>;
        })}
      </div>
      {!models.length && <Empty icon={<BarChart3 />} title="No model usage" text="Authenticated inference will group tokens by target here." />}
    </section>
    <section className="panel"><div className="panel-title"><div><h3>Usage by API key</h3><p>Revoked keys and legacy traffic remain attributable.</p></div></div><div className="usage-table"><div className="usage-head"><span>API key</span><span>Requests</span><span>Success</span><span>Latency</span><span>Input</span><span>Output</span></div>{usage.by_key.map(item => <div className="usage-row" key={item.api_key_id ?? "legacy"}><strong>{item.api_key_name}</strong><span>{formatNumber(item.request_count)}</span><span>{item.request_count ? `${Math.round(item.success_count / item.request_count * 100)}%` : "—"}</span><span>{Math.round(item.average_latency_ms)} ms</span><span>{formatNumber(item.input_tokens)}</span><span>{formatNumber(item.output_tokens)}</span></div>)}</div>{!usage.by_key.length && <Empty icon={<KeyRound />} title="No key usage" text="Usage will be grouped by the authenticating local key." />}</section>
  </>;
}

function ProvidersPage({ providers, providerPresets, refresh, success, fail }: Common) {
  const [editing, setEditing] = useState<Provider | null | undefined>();
  const remove = async (id: string) => { if (!confirm("Delete this provider and its cloud targets?")) return; try { await command("delete_provider", { id }); await refresh(); success("Provider removed"); } catch (e) { fail(e); } };
  const test = async (id: string) => { try { const models = await command<string[]>("test_provider_connection", { id }); success(`Connection successful${models.length > 1 ? ` · ${models.length} models visible` : ""}`); } catch (e) { fail(e); } };
  const connect = async (id: string) => { try { const start = await command<{ authorization_url: string }>("begin_openai_subscription", { id }); if (isTauri()) { await openUrl(start.authorization_url); } else { window.open(start.authorization_url, "_blank", "noopener"); } success("Browser opened; complete sign-in within three minutes"); } catch (e) { fail(e); } };
  const oauthStatus = async (id: string) => { try { const status = await command<{ state: string; account_id: string | null; error: string | null }>("openai_subscription_status", { id }); if (status.state === "error") fail(status.error ?? "OAuth failed"); else success(`OAuth status: ${status.state}${status.account_id ? ` · ${status.account_id}` : ""}`); await refresh(); } catch (e) { fail(e); } };
  const logout = async (id: string) => { try { await command("logout_openai_subscription", { id }); await refresh(); success("Subscription disconnected and tokens removed from Keychain"); } catch (e) { fail(e); } };
  return <>
    <PageHead eyebrow="Credentials" title="Providers" description="Keys stay in macOS Keychain and are never written to the database." action={<button className="primary" onClick={() => setEditing(null)}><Plus size={17} />Add provider</button>} />
    <div className="cards">{providers.map(provider => { const preset = providerPresets.find(item => item.id === provider.preset_id); const subscription = provider.auth_mode === "open_ai_subscription"; return <article className="provider-card" key={provider.id}><div className="provider-logo cloud"><Cloud /></div><div className="grow"><div className="row"><h3>{provider.name}</h3><Badge tone={provider.enabled && provider.has_credential ? "good" : "warn"}>{provider.enabled && provider.has_credential ? "Connected" : "Needs attention"}</Badge></div><p>{provider.base_url}</p><small>{preset?.name ?? provider.preset_id} · {accessLabel(preset?.access_tier)} · Credential {provider.has_credential ? "stored in Keychain" : "missing"}</small></div>{subscription ? <><button className="secondary" onClick={() => void oauthStatus(provider.id)}>Status</button>{!provider.has_credential && <button className="secondary" onClick={() => void connect(provider.id)}>Connect</button>}{provider.has_credential && <button className="secondary" onClick={() => void logout(provider.id)}>Logout</button>}</> : <button className="secondary" onClick={() => void test(provider.id)}>Test</button>}<button className="icon-button" onClick={() => setEditing(provider)}><Settings size={17} /></button><button className="icon-button danger" onClick={() => void remove(provider.id)}><Trash2 size={17} /></button></article>; })}</div>
    {!providers.length && <Empty icon={<KeyRound />} title="Connect your first provider" text="Choose a hosted provider or an experimental subscription connection." action={<button className="primary" onClick={() => setEditing(null)}><Plus size={17} />Add provider</button>} />}
    {editing !== undefined && <ProviderModal provider={editing} presets={providerPresets} close={() => setEditing(undefined)} done={async () => { setEditing(undefined); await refresh(); success("Provider saved"); }} fail={fail} />}
  </>;
}

function ProviderModal({ provider, presets, close, done, fail }: { provider: Provider | null; presets: ProviderPreset[]; close: () => void; done: () => Promise<void>; fail: (e: unknown) => void }) {
  const initialPreset = presets.find(item => item.id === provider?.preset_id) ?? presets[0];
  const [presetId, setPresetId] = useState(provider?.preset_id ?? initialPreset?.id ?? "openai"); const [name, setName] = useState(provider?.name ?? initialPreset?.name ?? "OpenAI"); const [url, setUrl] = useState(provider?.base_url ?? initialPreset?.base_url ?? ""); const [key, setKey] = useState(""); const [busy, setBusy] = useState(false);
  const preset = presets.find(item => item.id === presetId) ?? initialPreset;
  const choose = (next: string) => { const selected = presets.find(item => item.id === next); setPresetId(next); if (selected) { setName(selected.name); setUrl(selected.base_url ?? ""); } };
  const submit = async (event: FormEvent) => { event.preventDefault(); if (!preset) return; setBusy(true); try { await command("save_provider", { input: { id: provider?.id ?? null, name, presetId, authMode: preset.auth_mode, baseUrl: url, enabled: true, apiKey: key || null } }); await done(); } catch (e) { fail(e); } finally { setBusy(false); } };
  return <Modal title={provider ? "Edit provider" : "Add provider"} close={close}><form onSubmit={submit} className="form"><Field label="Provider preset"><select value={presetId} onChange={e => choose(e.target.value)}>{presets.map(item => <option value={item.id} key={item.id}>{item.name} · {accessLabel(item.access_tier)}</option>)}</select></Field><Field label="Display name"><input value={name} onChange={e => setName(e.target.value)} required /></Field><Field label="Base URL"><input value={url} onChange={e => setUrl(e.target.value)} required readOnly={!preset?.editable_base_url} /></Field>{preset?.auth_mode === "api_key" ? <Field label={provider?.has_credential ? "Replace API key (optional)" : "API key"}><input type="password" value={key} onChange={e => setKey(e.target.value)} required={!provider?.has_credential} placeholder="Provider API key" /></Field> : <div className="security-note"><CircleAlert size={17} /><span>Experimental ChatGPT subscription access is not a documented general OpenAI Platform API. API keys remain the stable application-access option.</span></div>}{preset?.note && <div className="security-note"><BookOpen size={17} /><span>{preset.note}</span></div>}{preset && <div className="security-note"><BookOpen size={17} /><a href={preset.docs_url} target="_blank" rel="noreferrer">Provider documentation</a></div>}<div className="security-note"><ShieldCheck size={17} /><span>Credentials are stored only in macOS Keychain. Connection testing, replacement and model sync are separate actions.</span></div><ModalActions close={close} busy={busy} label="Save provider" /></form></Modal>;
}

function parseDecimal(value: string): number | null {
  const trimmed = value.trim().replace(/\s/g, "");
  if (!trimmed) return null;
  const lastComma = trimmed.lastIndexOf(",");
  const lastDot = trimmed.lastIndexOf(".");
  let normalized = trimmed;
  if (lastComma >= 0 && lastDot >= 0) {
    normalized = lastComma > lastDot ? trimmed.replace(/\./g, "").replace(",", ".") : trimmed.replace(/,/g, "");
  } else if (lastComma >= 0) {
    normalized = `${trimmed.slice(0, lastComma).replace(/,/g, "")}.${trimmed.slice(lastComma + 1)}`;
  }
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : null;
}

function DecimalInput({ value, onChange, optional = false, min }: { value: number | null; onChange: (next: number | null) => void; optional?: boolean; min?: number }) {
  const [text, setText] = useState(value == null ? "" : String(value));
  return <input type="text" inputMode="decimal" min={min} value={text} onChange={event => {
    const raw = event.target.value;
    setText(raw);
    if (!raw.trim()) { if (optional) onChange(null); return; }
    const parsed = parseDecimal(raw);
    if (parsed != null) onChange(parsed);
  }} />;
}

function tokenPrice(input?: number | null, output?: number | null) {
  if (input == null || output == null) return null;
  const format = (value: number) => `$${value < 1 && value > 0 ? value.toFixed(2) : value % 1 ? value.toFixed(2) : value.toFixed(2)}`;
  return `${format(input)} / ${format(output)} per 1M`;
}

function CloudPage({ providers, targets, routingProfiles, refresh, success, fail }: Common) {
  const cloud = targets.filter(target => target.kind === "cloud");
  const [providerId, setProviderId] = useState(providers[0]?.id ?? ""); const [model, setModel] = useState(""); const [protocol, setProtocol] = useState<WireProtocol>("open_ai_chat"); const [capabilities, setCapabilities] = useState(["chat", "streaming"]); const [busy, setBusy] = useState(false);
  const queryClient = useQueryClient();
  const modelsQuery = useQuery({ queryKey: queryKeys.providerModels(providerId), queryFn: () => fetchers.providerModels(providerId), enabled: !!providerId });
  const models = modelsQuery.data ?? [];
  const selected = models.find(item => item.id === model);
  const metadataQuery = useQuery({
    queryKey: queryKeys.modelMetadata(model),
    queryFn: () => fetchers.modelMetadata(model),
    enabled: !!model.trim() && !selected,
  });
  useEffect(() => {
    if (selected) {
      setProtocol(selected.wire_protocol);
      setCapabilities(selected.capabilities);
      return;
    }
    if (metadataQuery.data && metadataQuery.data.source !== "fallback") setCapabilities(metadataQuery.data.capabilities);
  }, [model, selected, metadataQuery.data]);
  const sync = async () => { if (!providerId) return; setBusy(true); try { const found = await command<ProviderModel[]>("sync_provider_models", { id: providerId }); queryClient.setQueryData(queryKeys.providerModels(providerId), found); success(`${found.length} models discovered`); } catch (e) { fail(e); } finally { setBusy(false); } };
  const add = async () => { const provider = providers.find(item => item.id === providerId); if (!provider || !model) return; try { await command("save_target", { target: { id: crypto.randomUUID(), provider_id: provider.id, name: model, kind: "cloud", wire_protocol: protocol, provider_model: model, local_path: null, runtime_url: null, capabilities, enabled: true, state: "ready", size_bytes: null } }); setModel(""); await refresh(); success("Cloud model added"); } catch (e) { fail(e); } };
  const selectedPrice = tokenPrice(selected?.input_price_per_million, selected?.output_price_per_million);
  return <><PageHead eyebrow="Catalog" title="Cloud models" description="Features and list prices come from the provider API when available, otherwise from known-model defaults." />
    <section className="panel"><div className="toolbar-panel"><select value={providerId} onChange={e => setProviderId(e.target.value)}>{providers.map(provider => <option value={provider.id} key={provider.id}>{provider.name}</option>)}</select><button className="secondary" onClick={() => void sync()} disabled={!providerId || busy}>{busy ? <LoaderCircle className="spin" size={17} /> : <RefreshCw size={17} />}Sync catalog</button><div className="divider" /><input list="provider-models" value={model} onChange={e => setModel(e.target.value)} placeholder="Select or enter model ID" /><datalist id="provider-models">{models.map(item => <option value={item.id} key={item.id} />)}</datalist><select aria-label="Wire protocol" value={protocol} onChange={e => setProtocol(e.target.value as WireProtocol)}><option value="open_ai_chat">OpenAI Chat</option><option value="open_ai_responses">OpenAI Responses</option><option value="anthropic_messages">Anthropic Messages</option><option value="gemini_generate_content">Gemini GenerateContent</option></select><button className="primary" disabled={!model || !providerId} onClick={() => void add()}><Plus size={17} />Add model</button></div>{selectedPrice && <p className="catalog-hint">Known price {selectedPrice}</p>}<div className="capabilities capability-editor">{["chat", "streaming", "tools", "vision", "reasoning", "structured_output", "embeddings", "images", "audio", "audio_input", "video_input", "speech", "moderation"].map(item => <label key={item}><input type="checkbox" checked={capabilities.includes(item)} onChange={event => setCapabilities(event.target.checked ? [...capabilities, item] : capabilities.filter(value => value !== item))} />{item}</label>)}</div></section>
    <div className="table"><div className="table-head"><span>Model</span><span>Provider / protocol</span><span>Capabilities</span><span>Price</span><span>Status</span><span /></div>{cloud.map(target => { const profile = routingProfiles.find(item => item.target_id === target.id); const price = tokenPrice(profile?.input_price_per_million, profile?.output_price_per_million); return <div className="table-row" key={target.id}><strong>{target.name}</strong><span>{providers.find(p => p.id === target.provider_id)?.name ?? "Unknown"}<small>{protocolLabel(target.wire_protocol)}</small></span><CapabilityList items={target.capabilities} /><span>{price ?? "Unknown"}</span><Badge tone="good">Ready</Badge><DeleteTarget id={target.id} refresh={refresh} success={success} fail={fail} /></div>; })}</div>
    {!cloud.length && <Empty icon={<Cloud />} title="No cloud models selected" text="Sync a provider catalog or enter a model ID manually." />}
  </>;
}

function RoutesPage({ providers, targets, routes, publicModels, routingPolicies, routingProfiles, routingTasks, refresh, success, fail }: Common & { publicModels: PublicModel[] }) {
  const [editing, setEditing] = useState<ModelRoute | null | undefined>();
  const [policyRoute, setPolicyRoute] = useState<ModelRoute | null>(null);
  const [profileTarget, setProfileTarget] = useState<ModelTarget | null>(null);
  const [showConfig, setShowConfig] = useState(false);
  const remove = async (alias: string) => { try { await command("delete_route", { alias }); await refresh(); success("Alias deleted"); } catch (e) { fail(e); } };
  const setRoutingMode = async (route: ModelRoute, policy: RoutingPolicy | undefined, adaptive: boolean) => {
    try {
      await command("save_routing_policy", { policy: withAdaptiveEnabled(route, policy, adaptive) });
      await refresh();
      success(adaptive ? "Adaptive routing enabled for this alias" : "Performance routing enabled for this alias");
    } catch (error) { fail(error); }
  };
  return <><PageHead eyebrow="Routing" title="Custom routes" description="Optional named stacks with a primary pool and reserve fallbacks. Performance walks the pool in listed order; Adaptive ranks the primaries. Fallbacks are sequential failover after the pool is exhausted." action={<div className="button-row"><button className="secondary" onClick={() => setShowConfig(true)}><FileDown size={16} />Import / export</button><button className="primary" onClick={() => setEditing(null)} disabled={!targets.length}><Plus size={17} />Create route</button></div>} />
    <div className="route-list">
      <article className="route-card builtin-route-card">
        <div className="route-main"><div className="route-icon"><Sparkles /></div><div><div className="row"><h3>adaptive-routing</h3><Badge tone="good">Built-in</Badge></div><p>Always-on ranking across every enabled model by task quality, price, and the inferred task.</p></div></div>
      </article>
      {routes.map(route => { const policy = routingPolicies.find(item => item.alias === route.alias); const enabled = adaptiveEnabled(policy); const flow = [...hopsForRole(route, "primary").map((item, index, list) => ({ item, label: list.length > 1 ? `Primary ${index + 1}` : "Primary" })), ...hopsForRole(route, "fallback").map((item, index) => ({ item, label: `Fallback ${index + 1}` }))]; return <article className="route-card adaptive-route-card" key={route.alias}><div className="route-main"><div className="route-icon"><Route /></div><div><div className="row"><h3>{route.alias}</h3><Badge tone={enabled ? (policy?.status === "shadow" ? "warn" : "good") : "neutral"}>{enabled ? (policy?.status === "shadow" ? "Shadow" : "Adaptive") : "Performance"}</Badge></div><CapabilityList items={route.capabilities} /></div></div><div className="route-flow">{flow.map(({ item, label }, index) => <div key={`${item.id}-${index}`} className="route-target"><span>{label}</span><strong>{item.kind === "alias" ? item.id : (targets.find(target => target.id === item.id)?.name ?? item.model)}</strong></div>)}</div><div className="alias-adaptive"><RoutingModeSwitch alias={route.alias} adaptive={enabled} onChange={adaptive => void setRoutingMode(route, policy, adaptive)} /><button className="secondary compact" onClick={() => setPolicyRoute(route)}><Gauge size={15} />Configure</button></div><button className="icon-button" onClick={() => setEditing(route)}><Settings size={17} /></button><button className="icon-button danger" onClick={() => void remove(route.alias)}><Trash2 size={17} /></button></article>; })}
    </div>
    {!routes.length && <Empty icon={<Route />} title="No custom routes yet" text="Create a named stack when you need a primary pool and optional fallbacks." />}
    {editing !== undefined && <RouteModal route={editing} providers={providers} targets={targets} publicModels={publicModels} close={() => setEditing(undefined)} done={async () => { setEditing(undefined); await refresh(); success("Custom route saved"); }} fail={fail} />}
    {policyRoute && <RoutingPolicyModal route={policyRoute} policy={routingPolicies.find(item => item.alias === policyRoute.alias) ?? null} tasks={routingTasks} targets={targets} profiles={routingProfiles} onEditProfile={setProfileTarget} onTasksChanged={refresh} close={() => setPolicyRoute(null)} done={async () => { setPolicyRoute(null); await refresh(); success("Routing policy saved"); }} fail={fail} />}
    {profileTarget && <TargetProfileModal target={profileTarget} profile={routingProfiles.find(item => item.target_id === profileTarget.id) ?? null} tasks={routingTasks} close={() => setProfileTarget(null)} done={async () => { setProfileTarget(null); await refresh(); success("Target routing profile saved"); }} fail={fail} />}
    {showConfig && <RoutingConfigModal close={() => setShowConfig(false)} refresh={refresh} success={success} fail={fail} />}
  </>;
}

const defaultWeights = { quality: .55, cost: .15, latency: .15, reliability: .10, locality: .05 };
const defaultAdaptiveRules: RoutingPolicy["rules"] = [
  { id: "builtin-tools", task: "tool_use", priority: 10, endpoint_contains: null, has_tools: true, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: null },
  { id: "builtin-audio-video", task: "audio_video", priority: 20, endpoint_contains: null, has_tools: null, modalities_any: ["audio", "video"], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: null },
  { id: "builtin-vision", task: "vision", priority: 30, endpoint_contains: null, has_tools: null, modalities_any: ["vision"], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: null },
  { id: "builtin-reasoning", task: "reasoning", priority: 40, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: true, min_input_tokens: null, max_input_tokens: null, text_pattern: null },
  { id: "builtin-coding", task: "coding", priority: 50, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: "\\b(code|coding|function|debug|refactor|rust|python|typescript|sql|programm|implement)\\b" },
  { id: "builtin-summary", task: "summarization", priority: 60, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: "\\b(summarize|summary|summarise|zusammenfass|tl;?dr)\\b" },
  { id: "builtin-extraction", task: "extraction", priority: 70, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: "\\b(extract|parse|entities|extrahier|strukturier)\\b" },
  { id: "builtin-translation", task: "translation", priority: 80, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: "\\b(translate|translation|übersetz)\\b" },
  { id: "builtin-creative", task: "creative", priority: 90, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: "\\b(story|poem|creative|geschichte|gedicht|brainstorm)\\b" },
];

function hopRole(target: Pick<RouteTarget, "role">): RouteRole {
  return target.role === "fallback" ? "fallback" : "primary";
}

function hopsForRole(route: ModelRoute, role: RouteRole) {
  return [...route.targets].filter(target => hopRole(target) === role).sort((a, b) => a.priority - b.priority);
}

function primaryIds(route: ModelRoute) {
  return hopsForRole(route, "primary").map(target => target.id);
}

function scopedCandidates(route: ModelRoute, ids: string[]) {
  const hops = new Set(primaryIds(route));
  const kept = ids.filter(id => hops.has(id));
  return kept.length ? kept : primaryIds(route);
}

function defaultRoutingPolicy(route: ModelRoute): RoutingPolicy {
  return { version: 1, alias: route.alias, mode: "fixed", status: "draft", privacy: "local_preferred", default_task: "general", weights: defaultWeights, max_estimated_cost_usd: null, preferred_latency_ms: 2000, preferred_cost_usd: .01, candidate_target_ids: primaryIds(route), rules: defaultAdaptiveRules };
}

function adaptiveEnabled(policy?: RoutingPolicy | null) {
  return policy?.mode === "adaptive" && (policy.status === "active" || policy.status === "shadow");
}

function withAdaptiveEnabled(route: ModelRoute, policy: RoutingPolicy | null | undefined, enabled: boolean): RoutingPolicy {
  const next = { ...(policy ?? defaultRoutingPolicy(route)) };
  next.candidate_target_ids = scopedCandidates(route, next.candidate_target_ids);
  if (enabled) {
    next.mode = "adaptive";
    next.status = "active";
  } else {
    next.mode = "fixed";
    next.status = "draft";
  }
  return next;
}

function policyForSave(route: ModelRoute, policy: RoutingPolicy, expert: boolean): RoutingPolicy {
  return { ...policy, candidate_target_ids: expert ? scopedCandidates(route, policy.candidate_target_ids) : primaryIds(route) };
}

function policyWithScopedCandidates(route: ModelRoute, policy: RoutingPolicy): RoutingPolicy {
  return { ...policy, candidate_target_ids: scopedCandidates(route, policy.candidate_target_ids) };
}

function RoutingModeSwitch({ alias, adaptive, onChange }: { alias?: string; adaptive: boolean; onChange: (adaptive: boolean) => void }) {
  const performanceLabel = alias ? `Performance routing for ${alias}` : "Performance routing";
  const adaptiveLabel = alias ? `Adaptive routing for ${alias}` : "Adaptive routing";
  return <div className="segmented small" role="group" aria-label={alias ? `Routing mode for ${alias}` : "Routing mode"}>
    <button type="button" className={!adaptive ? "selected" : ""} aria-pressed={!adaptive} aria-label={performanceLabel} onClick={() => onChange(false)}>Performance</button>
    <button type="button" className={adaptive ? "selected" : ""} aria-pressed={adaptive} aria-label={adaptiveLabel} onClick={() => onChange(true)}>Adaptive</button>
  </div>;
}

function RoutingPolicyModal({ route, policy, tasks, targets, profiles, onEditProfile, onTasksChanged, close, done, fail }: { route: ModelRoute; policy: RoutingPolicy | null; tasks: RoutingTaskDefinition[]; targets: ModelTarget[]; profiles: TargetRoutingProfile[]; onEditProfile: (target: ModelTarget) => void; onTasksChanged: () => Promise<void>; close: () => void; done: () => Promise<void>; fail: (error: unknown) => void }) {
  const initial: RoutingPolicy = policyWithScopedCandidates(route, policy ?? defaultRoutingPolicy(route));
  const [value, setValue] = useState<RoutingPolicy>(structuredClone(initial)); const [expert, setExpert] = useState(false); const [busy, setBusy] = useState(false); const [sample, setSample] = useState(""); const [hint, setHint] = useState(""); const [simEndpoint, setSimEndpoint] = useState("/v1/chat/completions"); const [simTools, setSimTools] = useState(false); const [simReasoning, setSimReasoning] = useState(false); const [simModalities, setSimModalities] = useState(""); const [simMaxOutput, setSimMaxOutput] = useState(4096); const [preview, setPreview] = useState<RoutingEvaluation | null>(null);
  const [taskId, setTaskId] = useState(""); const [taskLabel, setTaskLabel] = useState("");
  const aliasTargets = route.targets.map(item => targets.find(target => target.id === item.id)).filter((target): target is ModelTarget => !!target);
  const savePolicy = async (event: FormEvent) => { event.preventDefault(); setBusy(true); try { await command("save_routing_policy", { policy: policyForSave(route, value, expert) }); await done(); } catch (error) { fail(error); } finally { setBusy(false); } };
  const simulate = async () => { try { setPreview(await command<RoutingEvaluation>("simulate_routing", { input: { alias: route.alias, policy: policyForSave(route, value, expert), task: hint || null, endpoint: simEndpoint, text: sample || null, hasTools: simTools, reasoning: simReasoning, modalities: simModalities.split(",").map(item => item.trim()).filter(Boolean), maxOutputTokens: simMaxOutput } })); } catch (error) { fail(error); } };
  const addRule = () => setValue({ ...value, rules: [...value.rules, { id: crypto.randomUUID(), task: value.default_task, priority: (value.rules.length + 1) * 10, endpoint_contains: null, has_tools: null, modalities_any: [], reasoning: null, min_input_tokens: null, max_input_tokens: null, text_pattern: null }] });
  const updateRule = (id: string, patch: Partial<RoutingPolicy["rules"][number]>) => setValue({ ...value, rules: value.rules.map(item => item.id === id ? { ...item, ...patch } : item) });
  const addTask = async () => { try { await command("save_routing_task", { task: { id: taskId, label: taskLabel, builtin: false } }); setTaskId(""); setTaskLabel(""); await onTasksChanged(); } catch (error) { fail(error); } };
  const deleteTask = async (id: string) => { try { await command("delete_routing_task", { id }); await onTasksChanged(); } catch (error) { fail(error); } };
  return <Modal title={`Routing · ${route.alias}`} wide action={<div className="segmented small" role="group" aria-label="Editor mode"><button type="button" className={!expert ? "selected" : ""} aria-pressed={!expert} onClick={() => setExpert(false)}>Easy</button><button type="button" className={expert ? "selected" : ""} aria-pressed={expert} onClick={() => setExpert(true)}>Expert</button></div>} close={close}><form className="form wide-form" onSubmit={savePolicy}>
    <RoutingModeSwitch adaptive={adaptiveEnabled(value)} onChange={enabled => setValue(withAdaptiveEnabled(route, value, enabled))} />
        <div className="security-note"><Gauge size={17} /><span>{adaptiveEnabled(value) ? "Adaptive ranks this alias's primary models by quality, price, and the inferred task. Fallbacks stay in reserve and run one by one after the pool is exhausted." : "Performance keeps the primary pool in listed order and skips slow, rate-limited, or failing primaries. Fallbacks run only after that, one by one, with a full timeout."}</span></div>
    <Field label="Privacy"><select aria-label="Privacy" value={value.privacy} onChange={event => setValue({ ...value, privacy: event.target.value as RoutingPolicy["privacy"] })}><option value="local_only">Local only</option><option value="local_preferred">Local preferred</option><option value="cloud_allowed">Cloud allowed</option></select></Field>
    {expert && <>
    <div className="three-fields"><Field label="Serving"><select aria-label="Adaptive serving" value={value.status === "shadow" ? "shadow" : "active"} disabled={!adaptiveEnabled(value)} onChange={event => setValue({ ...value, mode: "adaptive", status: event.target.value as RoutingPolicy["status"] })}><option value="active">Active · ranked models serve</option><option value="shadow">Shadow · fallbacks serve, adaptive logs</option></select></Field><Field label="Default task"><select aria-label="Default task" value={value.default_task} onChange={event => setValue({ ...value, default_task: event.target.value })}>{tasks.map(task => <option value={task.id} key={task.id}>{task.label}</option>)}</select></Field></div>
    <div><span className="field-label">Candidate primaries</span><div className="candidate-matrix">{hopsForRole(route, "primary").map(routeTarget => { const target = targets.find(item => item.id === routeTarget.id); const checked = value.candidate_target_ids.includes(routeTarget.id); return <label key={routeTarget.id}><input type="checkbox" checked={checked} onChange={() => setValue({ ...value, candidate_target_ids: checked ? value.candidate_target_ids.filter(id => id !== routeTarget.id) : [...value.candidate_target_ids, routeTarget.id] })} /><span>{target?.name ?? routeTarget.model}</span></label>; })}</div></div>
    <div className="routing-profiles"><div className="panel-title"><div><h3>Target profiles for this alias</h3><p>Context, pricing and task quality used when this alias ranks its models.</p></div></div><div className="profile-grid">{aliasTargets.map(target => { const profile = profiles.find(item => item.target_id === target.id); return <button type="button" className="profile-tile" key={target.id} onClick={() => onEditProfile(target)}><strong>{target.name}</strong><span>{profile ? `${profile.context_window.toLocaleString()} context · ${Object.keys(profile.task_quality).length} scores` : "Neutral defaults · configure"}</span>{target.kind === "cloud" && (!profile || profile.input_price_per_million == null || profile.output_price_per_million == null) && <Badge tone="warn">Price unknown</Badge>}</button>; })}</div></div>
    <div className="custom-tasks"><div><h3>Custom tasks</h3><p>Built-ins: {tasks.filter(task => task.builtin).map(task => task.id).join(", ")}</p><div className="custom-task-list">{tasks.filter(task => !task.builtin).map(task => <span key={task.id}>{task.label}<button type="button" className="icon-button" onClick={() => void deleteTask(task.id)}><X size={12} /></button></span>)}</div></div><div className="inline-form"><input value={taskId} onChange={event => setTaskId(event.target.value)} placeholder="task-id" /><input value={taskLabel} onChange={event => setTaskLabel(event.target.value)} placeholder="Display label" /><button type="button" className="secondary" disabled={!taskId || !taskLabel} onClick={() => void addTask()}><Plus size={15} />Add</button></div></div>
    <div><span className="field-label">Score weights</span><div className="weight-grid">{Object.entries(value.weights).map(([key, amount]) => <label key={key}><span>{key}</span><input aria-label={`${key} weight`} type="number" min="0" max="1" step="any" value={amount} onChange={event => { const next = parseDecimal(event.target.value); if (next != null) setValue({ ...value, weights: { ...value.weights, [key]: next } }); }} /></label>)}</div></div>
    <div className="three-fields"><Field label="Max estimated USD (blank = none)"><DecimalInput optional min={0} value={value.max_estimated_cost_usd} onChange={next => setValue({ ...value, max_estimated_cost_usd: next })} /></Field><Field label="Preferred latency ms"><input type="number" min="1" value={value.preferred_latency_ms} onChange={event => setValue({ ...value, preferred_latency_ms: Number(event.target.value) })} /></Field><Field label="Preferred cost USD"><DecimalInput min={0.000001} value={value.preferred_cost_usd} onChange={next => { if (next != null) setValue({ ...value, preferred_cost_usd: next }); }} /></Field></div>
    <div><span className="field-label">Simulator request metadata</span><div className="simulator-meta"><input aria-label="Simulator endpoint" value={simEndpoint} onChange={event => setSimEndpoint(event.target.value)} /><label><input type="checkbox" checked={simTools} onChange={event => setSimTools(event.target.checked)} />Tools</label><label><input type="checkbox" checked={simReasoning} onChange={event => setSimReasoning(event.target.checked)} />Reasoning</label><input aria-label="Simulator modalities" placeholder="vision,audio" value={simModalities} onChange={event => setSimModalities(event.target.value)} /><input aria-label="Simulator max output" type="number" min="1" value={simMaxOutput} onChange={event => setSimMaxOutput(Number(event.target.value))} /></div></div>
    <div><div className="panel-title"><div><h3>Ordered task rules</h3><p>All populated conditions must match. First match wins.</p></div><button type="button" className="text-button" onClick={addRule}><Plus size={14} />Rule</button></div>{value.rules.map((rule, index) => <div className="rule-card" key={rule.id}><div className="rule-row"><input type="number" aria-label={`Rule ${index + 1} priority`} value={rule.priority} onChange={event => updateRule(rule.id, { priority: Number(event.target.value) })} /><select value={rule.task} onChange={event => updateRule(rule.id, { task: event.target.value })}>{tasks.map(task => <option value={task.id} key={task.id}>{task.label}</option>)}</select><input placeholder="Text regex (optional)" value={rule.text_pattern ?? ""} onChange={event => updateRule(rule.id, { text_pattern: event.target.value || null })} /><button type="button" className="icon-button" onClick={() => setValue({ ...value, rules: value.rules.filter(item => item.id !== rule.id) })}><X size={14} /></button></div><div className="rule-conditions"><input placeholder="Endpoint contains" value={rule.endpoint_contains ?? ""} onChange={event => updateRule(rule.id, { endpoint_contains: event.target.value || null })} /><select value={rule.has_tools == null ? "" : String(rule.has_tools)} onChange={event => updateRule(rule.id, { has_tools: event.target.value === "" ? null : event.target.value === "true" })}><option value="">Any tools state</option><option value="true">Has tools</option><option value="false">No tools</option></select><select value={rule.reasoning == null ? "" : String(rule.reasoning)} onChange={event => updateRule(rule.id, { reasoning: event.target.value === "" ? null : event.target.value === "true" })}><option value="">Any reasoning state</option><option value="true">Reasoning requested</option><option value="false">No reasoning</option></select><input placeholder="Modalities: vision,audio" value={rule.modalities_any.join(",")} onChange={event => updateRule(rule.id, { modalities_any: event.target.value.split(",").map(item => item.trim()).filter(Boolean) })} /><input type="number" min="0" placeholder="Min tokens" value={rule.min_input_tokens ?? ""} onChange={event => updateRule(rule.id, { min_input_tokens: event.target.value ? Number(event.target.value) : null })} /><input type="number" min="0" placeholder="Max tokens" value={rule.max_input_tokens ?? ""} onChange={event => updateRule(rule.id, { max_input_tokens: event.target.value ? Number(event.target.value) : null })} /></div></div>)}</div>
    <section className="routing-simulator"><div><h3>Decision preview</h3><p>Example content stays in memory and is never logged.</p></div><div className="simulator-inputs"><select aria-label="Task hint" value={hint} onChange={event => setHint(event.target.value)}><option value="">No explicit task hint</option>{tasks.map(task => <option value={task.id} key={task.id}>{task.label}</option>)}</select><textarea aria-label="Routing sample" value={sample} onChange={event => setSample(event.target.value)} placeholder="Optional example prompt" /><button type="button" className="secondary" onClick={() => void simulate()}><Eye size={15} />Preview</button></div>{preview && <div className="decision-preview"><strong>{preview.mode} · {preview.task} via {preview.task_source}</strong>{preview.decision.ranked.map((candidate, index) => <span key={candidate.target_id}>{index + 1}. {targets.find(target => target.id === candidate.target_id)?.name ?? candidate.target_id} · {(candidate.score.total * 100).toFixed(1)} · {candidate.cost_verified ? `$${candidate.estimated_cost_usd?.toFixed(5)}` : "price unknown"}</span>)}{preview.decision.excluded.map(candidate => <span className="excluded" key={candidate.target_id}>{candidate.target_id}: {candidate.reason}</span>)}</div>}</section>
    <div className="security-note"><ShieldCheck size={17} /><span>Capabilities, privacy, context and known cost limits filter primaries before scoring. Fallbacks stay available for errors, outages, and missing features, and run in listed order with a full timeout.</span></div>
    </>}
    <ModalActions close={close} busy={busy} label="Save policy" />
  </form></Modal>;
}

function TargetProfileModal({ target, profile, tasks, close, done, fail }: { target: ModelTarget; profile: TargetRoutingProfile | null; tasks: RoutingTaskDefinition[]; close: () => void; done: () => Promise<void>; fail: (error: unknown) => void }) {
  const local = target.kind !== "cloud";
  const [value, setValue] = useState<TargetRoutingProfile>(profile ?? { version: 1, target_id: target.id, context_window: 8192, input_price_per_million: local ? 0 : null, output_price_per_million: local ? 0 : null, latency_prior_ms: local ? 1500 : 2000, reliability_prior: .95, task_quality: { general: 50 } }); const [busy, setBusy] = useState(false);
  const submit = async (event: FormEvent) => { event.preventDefault(); setBusy(true); try { await command("save_target_routing_profile", { profile: value }); await done(); } catch (error) { fail(error); } finally { setBusy(false); } };
  return <Modal title={`Routing profile · ${target.name}`} close={close}><form className="form" onSubmit={submit}><div className="two-fields"><Field label="Context window"><input type="number" min="1" value={value.context_window} onChange={event => setValue({ ...value, context_window: Number(event.target.value) })} /></Field><Field label="Latency prior ms"><input type="number" min="1" value={value.latency_prior_ms} onChange={event => setValue({ ...value, latency_prior_ms: Number(event.target.value) })} /></Field></div><div className="two-fields"><Field label="Input USD / 1M"><DecimalInput optional min={0} value={value.input_price_per_million} onChange={next => setValue({ ...value, input_price_per_million: next })} /></Field><Field label="Output USD / 1M"><DecimalInput optional min={0} value={value.output_price_per_million} onChange={next => setValue({ ...value, output_price_per_million: next })} /></Field></div><Field label="Reliability prior (0–1)"><input type="number" min="0" max="1" step="0.01" value={value.reliability_prior} onChange={event => setValue({ ...value, reliability_prior: Number(event.target.value) })} /></Field><div><span className="field-label">Quality by task (0–100)</span><div className="quality-grid">{tasks.map(task => <label key={task.id}><span>{task.label}</span><input aria-label={`${task.label} quality`} type="number" min="0" max="100" value={value.task_quality[task.id] ?? ""} onChange={event => { const next = { ...value.task_quality }; if (event.target.value) next[task.id] = Number(event.target.value); else delete next[task.id]; setValue({ ...value, task_quality: next }); }} /></label>)}</div></div>{target.kind === "cloud" && (value.input_price_per_million == null || value.output_price_per_million == null) && <div className="security-note warning-note"><CircleAlert size={17} /><span>Unknown pricing makes this target a last-resort candidate and prevents a guaranteed request budget.</span></div>}<ModalActions close={close} busy={busy} label="Save profile" /></form></Modal>;
}

function RoutingConfigModal({ close, refresh, success, fail }: { close: () => void; refresh: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [json, setJson] = useState(""); const [preview, setPreview] = useState<string[]>([]); const [busy, setBusy] = useState(false);
  const exportQuery = useQuery({ queryKey: queryKeys.routingConfig, queryFn: fetchers.routingConfig });
  useEffect(() => { if (exportQuery.data) setJson(JSON.stringify(exportQuery.data, null, 2)); }, [exportQuery.data]);
  useEffect(() => { if (exportQuery.error) fail(exportQuery.error); }, [exportQuery.error, fail]);
  const run = async (apply: boolean) => { setBusy(true); try { const config = JSON.parse(json) as RoutingConfigExport; const result = await command<{ valid: boolean; task_count: number; profile_count: number; policy_count: number; warnings: string[] }>("import_routing_config", { config, apply }); setPreview([`${result.task_count} tasks · ${result.profile_count} profiles · ${result.policy_count} policies`, ...result.warnings]); if (apply) { await refresh(); success("Routing configuration imported atomically"); close(); } } catch (error) { fail(error); } finally { setBusy(false); } };
  return <Modal title="Routing policy JSON" close={close}><div className="form"><textarea className="config-json" aria-label="Routing configuration JSON" value={json} onChange={event => setJson(event.target.value)} />{preview.map(item => <small className="config-warning" key={item}>{item}</small>)}<div className="security-note"><ShieldCheck size={17} /><span>Exports exclude credentials, request history, prompts and responses. Preview validates every reference before changes are applied.</span></div><div className="modal-actions"><button className="secondary" onClick={close}>Cancel</button><button className="secondary" disabled={busy} onClick={() => void run(false)}>Validate preview</button><button className="primary" disabled={busy} onClick={() => void run(true)}>Apply import</button></div></div></Modal>;
}

function hopOrigin(target: ModelTarget, providers: Provider[]) {
  if (target.kind === "cloud") return { origin: providers.find(provider => provider.id === target.provider_id)?.name ?? "Cloud", detail: undefined as string | undefined };
  return { origin: target.kind === "gguf" ? "Local · GGUF" : "Local · MLX", detail: target.source_repo ?? undefined };
}

function RouteModal({ route, providers, targets, publicModels, close, done, fail }: { route: ModelRoute | null; providers: Provider[]; targets: ModelTarget[]; publicModels: PublicModel[]; close: () => void; done: () => Promise<void>; fail: (e: unknown) => void }) {
  const hopOptions = useMemo(() => [
    ...targets.map(target => {
      const { origin, detail } = hopOrigin(target, providers);
      return { value: target.id, label: target.name, origin, detail, search: [target.name, origin, detail, target.provider_model, target.kind].filter(Boolean).join(" "), kind: target.kind as TargetKind, model: target.provider_model, capabilities: target.capabilities };
    }),
    ...publicModels.filter(model => (model.source === "alias" || model.source === "adaptive") && model.id !== (route?.alias ?? "")).map(model => {
      const origin = model.source === "adaptive" ? "Built-in" : "Custom route";
      return { value: model.id, label: model.id, origin, detail: undefined as string | undefined, search: `${model.id} ${origin} alias`, kind: "alias" as TargetKind, model: model.id, capabilities: model.capabilities };
    }),
  ], [targets, publicModels, providers, route?.alias]);
  const [alias, setAlias] = useState(route?.alias ?? "");
  const [primaries, setPrimaries] = useState<string[]>(route ? hopsForRole(route, "primary").map(item => item.id) : [hopOptions[0]?.value].filter(Boolean));
  const [fallbacks, setFallbacks] = useState<string[]>(route ? hopsForRole(route, "fallback").map(item => item.id) : []);
  const [busy, setBusy] = useState(false);
  const used = [...primaries, ...fallbacks];
  const nextHop = hopOptions.find(hop => !used.includes(hop.value))?.value;
  const availableCapabilities = useMemo(() => {
    const seen = new Set<string>();
    for (const id of [...primaries, ...fallbacks]) {
      for (const capability of hopOptions.find(hop => hop.value === id)?.capabilities ?? []) seen.add(capability);
    }
    return [...seen];
  }, [primaries, fallbacks, hopOptions]);
  const hopRow = (id: string, index: number, role: RouteRole, list: string[], setList: (next: string[]) => void, removable: boolean) => {
    const options = hopOptions.filter(hop => hop.value === id || !used.includes(hop.value));
    const label = role === "fallback" ? `Fallback ${index + 1}` : (list.length > 1 ? `Primary ${index + 1}` : "Primary");
    return <div className="picker-row" key={`${role}-${id}-${index}`}><span>{label}</span><TypeaheadSelect ariaLabel={label} value={id} options={options} onChange={next => setList(list.map((item, itemIndex) => itemIndex === index ? next : item))} />{removable && <button type="button" className="icon-button" onClick={() => setList(list.filter((_, itemIndex) => itemIndex !== index))}><X size={15} /></button>}</div>;
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      const targetsForSave = [
        ...primaries.map((id, index) => {
          const hop = hopOptions.find(item => item.value === id);
          return { id, kind: hop?.kind ?? "cloud", model: hop?.model ?? id, priority: (index + 1) * 10, enabled: true, role: "primary" as const };
        }),
        ...fallbacks.map((id, index) => {
          const hop = hopOptions.find(item => item.value === id);
          return { id, kind: hop?.kind ?? "cloud", model: hop?.model ?? id, priority: (index + 1) * 10, enabled: true, role: "fallback" as const };
        }),
      ];
      await command("save_route", { route: { alias, enabled: true, capabilities: availableCapabilities, targets: targetsForSave } });
      await done();
    } catch (e) { fail(e); } finally { setBusy(false); }
  };
  return <Modal title={route ? "Edit custom route" : "Create custom route"} close={close}><form className="form" onSubmit={submit}><Field label="Public model name"><input value={alias} onChange={e => setAlias(e.target.value.replace(/\s+/g, "-"))} placeholder="my-assistant" required disabled={!!route} /></Field><div><span className="field-label">Primary pool</span><div className="target-picker">{primaries.map((id, index) => hopRow(id, index, "primary", primaries, setPrimaries, primaries.length > 1))}<button type="button" className="text-button" disabled={!nextHop} onClick={() => nextHop && setPrimaries([...primaries, nextHop])}><Plus size={15} />Add primary</button></div></div><div><span className="field-label">Fallbacks</span><div className="target-picker">{fallbacks.map((id, index) => hopRow(id, index, "fallback", fallbacks, setFallbacks, true))}<button type="button" className="text-button" disabled={!nextHop} onClick={() => nextHop && setFallbacks([...fallbacks, nextHop])}><Plus size={15} />Add fallback</button></div></div><div><span className="field-label">Advertised capabilities</span><CapabilityList items={availableCapabilities} /></div><div className="security-note"><ListRestart size={17} /><span>Fallbacks run only on errors, unreachability, or a missing feature such as vision—never after streaming has started. Slow, 404 and 429 skips still apply inside the primary pool.</span></div><ModalActions close={close} busy={busy} label="Save route" /></form></Modal>;
}

function LogsPage({ localKeys, refresh, success, fail }: Common) {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(0);
  const [text, setText] = useState(""); const [keyFilter, setKeyFilter] = useState(""); const [alias, setAlias] = useState(""); const [target, setTarget] = useState(""); const [endpoint, setEndpoint] = useState(""); const [status, setStatus] = useState(""); const [from, setFrom] = useState(""); const [to, setTo] = useState("");
  const debouncedText = useDebouncedValue(text, 180);
  const logQuery = (withPage = true): LogQuery => ({
    query: debouncedText || null, api_key_id: keyFilter && keyFilter !== "legacy" ? keyFilter : null, legacy_only: keyFilter === "legacy",
    alias: alias || null, target: target || null, endpoint: endpoint || null, status_class: (status || null) as LogQuery["status_class"],
    from: from ? new Date(from).toISOString() : null, to: to ? new Date(to).toISOString() : null,
    ...(withPage ? { limit: 50, offset: page * 50 } : {}),
  });
  const query = logQuery();
  const facetsQuery = useQuery({ queryKey: queryKeys.logFacets, queryFn: fetchers.logFacets });
  const logsQuery = useQuery({ queryKey: queryKeys.logs(query), queryFn: () => fetchers.logs(query), placeholderData: keepPreviousData });
  const facets = facetsQuery.data ?? { aliases: [], targets: [], endpoints: [] };
  const items = logsQuery.data?.items ?? [];
  const total = logsQuery.data?.total ?? 0;
  useEffect(() => { if (facetsQuery.error) fail(facetsQuery.error); }, [facetsQuery.error, fail]);
  useEffect(() => { if (logsQuery.error) fail(logsQuery.error); }, [logsQuery.error, fail]);
  const updateFilter = (setter: (value: string) => void, value: string) => { setter(value); setPage(0); };
  const reset = () => { setText(""); setKeyFilter(""); setAlias(""); setTarget(""); setEndpoint(""); setStatus(""); setFrom(""); setTo(""); setPage(0); };
  const clear = async () => { if (!confirm("Delete all request metadata?")) return; try { await command("clear_logs"); await queryClient.invalidateQueries({ queryKey: ["logs"] }); await queryClient.invalidateQueries({ queryKey: queryKeys.logFacets }); await refresh(); success("Logs cleared"); } catch (e) { fail(e); } };
  const exportCsv = async () => {
    try {
      if (!isTauri()) {
        const csv = await command<string>("export_logs_csv", { path: null, query: logQuery(false) });
        downloadTextFile("local-ai-router-logs.csv", csv, "text/csv");
        success(`${total} matching logs exported`);
        return;
      }
      const path = await save({ defaultPath: "local-ai-router-logs.csv", filters: [{ name: "CSV", extensions: ["csv"] }] });
      if (!path) return;
      await command("export_logs_csv", { path, query: logQuery(false) });
      success(`${total} matching logs exported`);
    } catch (e) { fail(e); }
  };
  return <><PageHead eyebrow="Observability" title="Request logs" description="Metadata only. Prompt and response content is never stored." action={<div className="button-row"><button className="secondary" onClick={() => void exportCsv()}><FileDown size={16} />Export filtered CSV</button><button className="secondary danger-text" onClick={() => void clear()}><Trash2 size={16} />Clear</button></div>} />
    <section className="panel log-filters"><div className="search"><Search size={17} /><input value={text} onChange={e => updateFilter(setText, e.target.value)} placeholder="Search endpoint, key, alias, target, status or error…" /></div><div className="filter-grid">
      <select aria-label="API key" value={keyFilter} onChange={e => updateFilter(setKeyFilter, e.target.value)}><option value="">All API keys</option><option value="legacy">Unknown / Legacy</option>{localKeys.map(key => <option key={key.id} value={key.id}>{key.name}{key.revoked_at ? " (revoked)" : ""}</option>)}</select>
      <select aria-label="Alias" value={alias} onChange={e => updateFilter(setAlias, e.target.value)}><option value="">All aliases</option>{facets.aliases.map(value => <option key={value}>{value}</option>)}</select>
      <select aria-label="Target" value={target} onChange={e => updateFilter(setTarget, e.target.value)}><option value="">All targets</option>{facets.targets.map(value => <option key={value}>{value}</option>)}</select>
      <select aria-label="Endpoint" value={endpoint} onChange={e => updateFilter(setEndpoint, e.target.value)}><option value="">All endpoints</option>{facets.endpoints.map(value => <option key={value} value={value}>{value.replace("/v1/", "")}</option>)}</select>
      <select aria-label="Status" value={status} onChange={e => updateFilter(setStatus, e.target.value)}><option value="">All statuses</option><option value="success">Success</option><option value="4xx">4xx</option><option value="5xx">5xx</option></select>
      <label className="date-filter"><span>From</span><input type="datetime-local" value={from} onChange={e => updateFilter(setFrom, e.target.value)} /></label><label className="date-filter"><span>To</span><input type="datetime-local" value={to} onChange={e => updateFilter(setTo, e.target.value)} /></label><button className="secondary" onClick={reset}><X size={15} />Reset</button>
    </div></section>
    <div className="result-meta"><span>{total} matching request{total === 1 ? "" : "s"}</span><span>Page {page + 1} of {Math.max(1, Math.ceil(total / 50))}</span></div>
    <div className="log-table"><div className="log-head"><span>Time</span><span>API key</span><span>Endpoint</span><span>Route</span><span>Status</span><span>Latency</span><span>Tokens</span><span>Attempts</span></div>{items.map(log => <div className="log-row" key={log.id}><span>{new Date(log.created_at).toLocaleString()}</span><strong>{log.api_key_name ?? "Unknown / Legacy"}</strong><code>{log.endpoint.replace("/v1/", "")}</code><span><strong>{log.alias ?? "—"}</strong><small>{log.target ?? "No target"}</small></span><span><Badge tone={statusTone(log.status)}>{log.status}</Badge>{(log.error_code || log.error_message) && <small>{[log.error_code, log.error_message].filter(Boolean).join(" · ")}</small>}</span><span>{log.latency_ms} ms</span><span>{log.input_tokens == null || log.output_tokens == null ? "—" : formatNumber(log.input_tokens + log.output_tokens)}</span><span>{log.attempts}{recoveryLabel(log.attempts, log.status) && <small>{recoveryLabel(log.attempts, log.status)}</small>}</span></div>)}</div>{!items.length && <Empty icon={<Activity />} title="No matching requests" text="Adjust the filters or make an authenticated request." />}
    {total > 50 && <div className="pagination"><button className="secondary" disabled={page === 0} onClick={() => setPage(page - 1)}>Previous</button><button className="secondary" disabled={(page + 1) * 50 >= total} onClick={() => setPage(page + 1)}>Next</button></div>}
  </>;
}

function RoutingLogsPage({ fail }: { fail: (error: unknown) => void }) {
  const [alias, setAlias] = useState("");
  const attemptsQuery = useQuery({ queryKey: queryKeys.routingAttempts, queryFn: fetchers.routingAttempts });
  useEffect(() => { if (attemptsQuery.error) fail(attemptsQuery.error); }, [attemptsQuery.error, fail]);
  const items = attemptsQuery.data ?? [];
  const aliases = [...new Set(items.map(item => item.alias))];
  const visible = items.filter(item => !alias || item.alias === alias);
  return <><PageHead eyebrow="Observability" title="Routing" description="Decision metadata for adaptive ranking, fallbacks, rate limits and slow-model avoidance. Prompt and response content is never stored." />
    <section className="panel log-filters"><div className="filter-grid"><select aria-label="Alias" value={alias} onChange={e => setAlias(e.target.value)}><option value="">All aliases</option>{aliases.map(value => <option key={value}>{value}</option>)}</select></div></section>
    <div className="result-meta"><span>{visible.length} attempt{visible.length === 1 ? "" : "s"}</span></div>
    <div className="log-table routing-table"><div className="log-head routing-head"><span>Time</span><span>Alias / task</span><span>Target</span><span>Mode</span><span>Status</span><span>Latency</span><span>Score</span><span>Reason</span></div>
      {visible.map(attempt => <div className="log-row routing-row" key={attempt.id}><span>{new Date(attempt.created_at).toLocaleString()}</span><span><strong>{attempt.alias}</strong><small>{attempt.task} · {attempt.task_source}</small></span><span><strong>{attempt.target_id}</strong><small>{routingAttemptLabel(attempt)}</small></span><code>{attempt.routing_mode}</code><Badge tone={attempt.status < 400 ? "good" : attempt.status === 404 || attempt.status === 429 ? "warn" : "bad"}>{attempt.status}</Badge><span>{attempt.ttft_ms != null ? `${attempt.ttft_ms} ms TTFT` : `${attempt.latency_ms} ms`}</span><span>{attempt.score ? (attempt.score.total * 100).toFixed(1) : "—"}</span><small className="routing-reason">{attempt.reason}</small></div>)}
    </div>
    {!visible.length && <Empty icon={<Sparkles />} title="No routing attempts" text="Call adaptive-routing or a custom alias to record ranking and fallback decisions." />}
  </>;
}

function SettingsPage({ settings, resourcePolicy, dashboard, refresh, success, fail }: Common & { dashboard: DashboardData }) {
  const [autostart, setAutostart] = useState(false); const [hf, setHf] = useState(""); const [civitai, setCivitai] = useState(""); const [policy, setPolicy] = useState(resourcePolicy);
  const [bindMode, setBindMode] = useState(settings.bind_mode ?? "loopback");
  const [bindAddress, setBindAddress] = useState(settings.bind_address ?? "");
  const [tlsCert, setTlsCert] = useState(settings.tls_cert_path ?? "");
  const [tlsKey, setTlsKey] = useState(settings.tls_key_path ?? "");
  const [githubId, setGithubId] = useState(""); const [githubSecret, setGithubSecret] = useState("");
  const [googleId, setGoogleId] = useState(""); const [googleSecret, setGoogleSecret] = useState("");
  useEffect(() => { if (isTauri()) { void isEnabled().then(setAutostart).catch(() => {}); } }, []);
  useEffect(() => setPolicy(resourcePolicy), [resourcePolicy]);
  useEffect(() => { setBindMode(settings.bind_mode ?? "loopback"); setBindAddress(settings.bind_address ?? ""); setTlsCert(settings.tls_cert_path ?? ""); setTlsKey(settings.tls_key_path ?? ""); }, [settings]);
  const toggleAutostart = async () => { try { autostart ? await disable() : await enable(); setAutostart(!autostart); success(`Launch at login ${autostart ? "disabled" : "enabled"}`); } catch (e) { fail(e); } };
  const saveNumber = async (key: string, value: string) => { try { await command("save_setting", { key, value }); await refresh(); success("Setting saved; runtime changes apply after restart"); } catch (e) { fail(e); } };
  const savePolicy = async (next: ResourcePolicy) => { setPolicy(next); try { await command("save_resource_policy", { policy: next }); await refresh(); success("Resource policy saved; loaded models restart after active requests finish"); } catch (e) { setPolicy(resourcePolicy); fail(e); } };
  const updatePolicy = (change: Partial<ResourcePolicy>) => setPolicy(current => ({ ...current, ...change, profile: "custom" }));
  const chooseProfile = async (profile: ResourceProfile) => { if (profile === "custom") return; try { const preset = await command<ResourcePolicy>("get_resource_profile_preset", { profile }); await savePolicy(preset); } catch (error) { fail(error); } };
  const memoryWarning = dashboard.runtimes.some(runtime => runtime.memory_warning);
  return <><PageHead eyebrow="Application" title="Settings" description="Security, resources and background behavior." />
    {memoryWarning && <div className="security-note"><CircleAlert size={17} /><span>Resident local runtimes currently exceed the soft memory budget. Active responses are allowed to finish.</span></div>}
    <div className="settings-list"><Setting title="Local endpoint" description={bindMode === "loopback" ? "The gateway is bound to 127.0.0.1 over HTTP. Enable LAN share to reach other machines." : "Non-loopback binds require HTTPS. Restart the app or serve process after changing bind settings."}><code>{dashboard.base_url}</code></Setting>
      <Setting title="Network share" description="Loopback stays the default. LAN or a specific address requires HTTPS (auto-generated self-signed cert, or your own files). Inference still needs a local API key.">
        <div className="resource-controls">
          <select aria-label="Bind mode" value={bindMode} onChange={event => { const value = event.target.value; setBindMode(value); void saveNumber("bind_mode", value); }}><option value="loopback">Loopback (HTTP)</option><option value="lan">LAN (all interfaces, HTTPS)</option><option value="address">Specific address (HTTPS unless loopback)</option></select>
          {bindMode === "address" && <input value={bindAddress} onChange={event => setBindAddress(event.target.value)} onBlur={() => void saveNumber("bind_address", bindAddress)} placeholder="192.168.1.10" />}
        </div>
      </Setting>
      {(bindMode !== "loopback" || settings.tls_fingerprint) && <Setting title="TLS certificate" description="Leave empty to auto-generate a self-signed certificate in the data directory. Pin the fingerprint in clients and browsers.">
        <div className="resource-controls" style={{ flexDirection: "column", alignItems: "flex-end" }}>
          {settings.tls_fingerprint && <code>{settings.tls_fingerprint}</code>}
          <input value={tlsCert} onChange={event => setTlsCert(event.target.value)} onBlur={() => void saveNumber("tls_cert_path", tlsCert)} placeholder="Certificate PEM path (optional)" />
          <input value={tlsKey} onChange={event => setTlsKey(event.target.value)} onBlur={() => void saveNumber("tls_key_path", tlsKey)} placeholder="Private key PEM path (optional)" />
        </div>
      </Setting>}
      {isTauri() && <Setting title="Launch at login" description="Keep the menu bar gateway available after signing in."><Toggle checked={autostart} onChange={() => void toggleAutostart()} /></Setting>}
      <Setting title="Inference profile" description="Stealth caps runnable inference time to 25%; this is not an exact Metal GPU utilization quota."><select aria-label="Inference profile" value={policy.profile} onChange={event => void chooseProfile(event.target.value as ResourceProfile)}><option value="stealth">Stealth</option><option value="balanced">Balanced</option><option value="performance">Performance</option><option value="custom">Custom</option></select></Setting>
      <Setting title="Soft memory budget" description="Blocks new model admission and unloads idle models; an active response is never killed."><div className="resource-controls"><ResourceNumber label="Percent" value={policy.memory_budget_percent} min={10} max={95} suffix="%" onChange={value => updatePolicy({ memory_budget_percent: value })} onSave={() => void savePolicy(policy)} /><ResourceNumber label="Absolute cap" value={policy.memory_budget_mib ?? 0} min={0} max={1048576} suffix="MiB (0 = off)" onChange={value => updatePolicy({ memory_budget_mib: value || null })} onSave={() => void savePolicy(policy)} /></div></Setting>
      <Setting title="Compute duty cycle" description="Sidecars may run for this share of each 400 ms window. Short GPU bursts can exceed this percentage."><ResourceNumber label="Duty" value={policy.compute_duty_percent} min={5} max={100} suffix="%" onChange={value => updatePolicy({ compute_duty_percent: value })} onSave={() => void savePolicy(policy)} /></Setting>
      <Setting title="CPU and prompt concurrency" description="Waiting prompts use an authenticated FIFO queue without a router timeout."><div className="resource-controls"><ResourceNumber label="CPU threads" value={policy.cpu_threads} min={1} max={128} suffix="threads" onChange={value => updatePolicy({ cpu_threads: value })} onSave={() => void savePolicy(policy)} /><ResourceNumber label="Parallel prompts" value={policy.max_parallel_prompts} min={1} max={16} suffix="active" onChange={value => updatePolicy({ max_parallel_prompts: value, disk_kv_enabled: value === 1 ? policy.disk_kv_enabled : false })} onSave={() => void savePolicy(policy)} /><ResourceNumber label="Process priority" value={policy.process_priority} min={-1} max={2} suffix="-1 = background" onChange={value => updatePolicy({ process_priority: value })} onSave={() => void savePolicy(policy)} /></div></Setting>
      <Setting title="Automatic load / unload" description="Load a stopped model on its first request and release it after the idle period."><div className="resource-controls"><Toggle checked={policy.auto_load} onChange={() => void savePolicy({ ...policy, profile: "custom", auto_load: !policy.auto_load })} /><ResourceNumber label="Idle" value={policy.idle_unload_minutes} min={0} max={1440} suffix="min (0 = off)" onChange={value => updatePolicy({ idle_unload_minutes: value })} onSave={() => void savePolicy(policy)} /></div></Setting>
      <Setting title="GPU / NPU" description="GGUF GPU layers are configurable. MLX uses Metal; Apple Neural Engine quotas are not exposed by the current engines."><ResourceNumber label="GGUF layers" value={policy.gguf_gpu_layers} min={-1} max={999} suffix="-1 = auto" onChange={value => updatePolicy({ gguf_gpu_layers: value })} onSave={() => void savePolicy(policy)} /></Setting>
      <Setting title="Persistent local KV" description="Local chat models (GGUF and MLX) can keep KV snapshots on disk. GGUF needs X-Local-AI-Session. MLX reuses prefixes in RAM and restores from token-block hashes without a session; the header is optional isolation. Files are unencrypted, private to the app, and limited to 10 GiB."><div className="resource-controls"><Toggle checked={policy.disk_kv_enabled} onChange={() => void savePolicy({ ...policy, profile: "custom", disk_kv_enabled: !policy.disk_kv_enabled, max_parallel_prompts: !policy.disk_kv_enabled ? 1 : policy.max_parallel_prompts })} /><button className="secondary danger-text" onClick={async () => { if (!confirm("Delete all persistent KV snapshots?")) return; try { await command("clear_kv_cache", { targetId: null }); success("Persistent KV snapshots deleted"); } catch (e) { fail(e); } }}><Trash2 size={15} />Clear cache</button></div></Setting>
      <Setting title="Log retention" description="Metadata older than this is removed automatically."><NumberSetting value={settings.log_retention_days ?? "30"} suffix="days" onSave={value => void saveNumber("log_retention_days", value)} /></Setting>
      <Setting title="Hugging Face token" description="Optional for gated and private model repositories."><div className="inline-form"><input type="password" value={hf} onChange={e => setHf(e.target.value)} placeholder={settings.has_hf_token === "true" ? "Token stored in Keychain" : "hf_…"} /><button className="secondary" onClick={async () => { try { await command("save_hugging_face_token", { token: hf }); setHf(""); await refresh(); success("Hugging Face token saved"); } catch (e) { fail(e); } }}>Save</button></div></Setting>
      <Setting title="CivitAI token" description="Optional for CivitAI checkpoint downloads."><div className="inline-form"><input type="password" value={civitai} onChange={e => setCivitai(e.target.value)} placeholder={settings.has_civitai_token === "true" ? "Token stored in Keychain" : "API token"} /><button className="secondary" onClick={async () => { try { await command("save_civitai_token", { token: civitai }); setCivitai(""); await refresh(); success("CivitAI token saved"); } catch (e) { fail(e); } }}>Save</button></div></Setting>
      <Setting title="GitHub OpenID" description="Allowlisted GitHub accounts can sign in to the admin UI. Callback is /auth/oidc/callback on this gateway."><div className="inline-form"><input value={githubId} onChange={e => setGithubId(e.target.value)} placeholder={settings.has_github_oidc === "true" ? "Client ID stored" : "Client ID"} /><input type="password" value={githubSecret} onChange={e => setGithubSecret(e.target.value)} placeholder="Client secret" /><button className="secondary" onClick={async () => { try { await command("save_oidc_client", { provider: "github", clientId: githubId, clientSecret: githubSecret }); setGithubSecret(""); await refresh(); success("GitHub OpenID saved"); } catch (e) { fail(e); } }}>Save</button></div></Setting>
      <Setting title="Google OpenID" description="Allowlisted Google accounts can sign in. Invite emails on the Users page first."><div className="inline-form"><input value={googleId} onChange={e => setGoogleId(e.target.value)} placeholder={settings.has_google_oidc === "true" ? "Client ID stored" : "Client ID"} /><input type="password" value={googleSecret} onChange={e => setGoogleSecret(e.target.value)} placeholder="Client secret" /><button className="secondary" onClick={async () => { try { await command("save_oidc_client", { provider: "google", clientId: googleId, clientSecret: googleSecret }); setGoogleSecret(""); await refresh(); success("Google OpenID saved"); } catch (e) { fail(e); } }}>Save</button></div></Setting></div>
  </>;
}

function ModelRow({ model, runtime, compact }: { model: ModelTarget; runtime?: DashboardData["runtimes"][number]; compact?: boolean }) {
  const tps = runtime?.tokens_per_second != null ? `${runtime.tokens_per_second.toFixed(1)} tok/s · ` : "";
  return <div className="model-row"><div className="model-icon small"><Box size={17} /></div><div className="grow"><strong>{model.name}</strong><span>{runtime ? `${tps}${runtime.profile} · ${runtime.compute_duty_percent}% duty · ${formatBytes(runtime.resident_bytes)} RSS · ${runtime.active} active / ${runtime.queued} queued${runtime.pending_restart ? " · restart pending" : ""}` : `${model.kind.toUpperCase()} ${model.size_bytes ? `· ${formatBytes(model.size_bytes)}` : ""}`}</span></div><Badge tone={runtime?.memory_warning ? "warn" : model.state === "ready" ? "good" : "neutral"}>{runtime?.memory_warning ? "Over budget" : compact && model.state === "stopped" ? "Installed" : model.state}</Badge></div>;
}
function Metric({ icon, value, label }: { icon: ReactNode; value: ReactNode; label: string }) { return <div className="metric"><span>{icon}</span><div><strong>{value}</strong><small>{label}</small></div></div>; }
function CapabilityList({ items }: { items: string[] }) { return <div className="capabilities">{items.slice(0, 4).map(item => <span key={item}>{item}</span>)}</div>; }
function Badge({ children, tone }: { children: ReactNode; tone: "good" | "warn" | "bad" | "neutral" }) { return <span className={`badge ${tone}`}>{children}</span>; }
function Field({ label, children }: { label: string; children: ReactNode }) { return <label className="field"><span>{label}</span>{children}</label>; }
function Setting({ title, description, children }: { title: string; description: string; children: ReactNode }) { return <section className="setting"><div><h3>{title}</h3><p>{description}</p></div><div>{children}</div></section>; }
function Empty({ icon, title, text, action }: { icon: ReactNode; title: string; text: string; action?: ReactNode }) { return <div className="empty"><div>{icon}</div><h3>{title}</h3><p>{text}</p>{action}</div>; }
function Loading() { return <div className="loading"><LoaderCircle className="spin" /><span>Loading private gateway…</span></div>; }
function Modal({ title, close, children, wide, action }: { title: string; close: () => void; children: ReactNode; wide?: boolean; action?: ReactNode }) { return <div className="modal-backdrop" onMouseDown={close}><div className={wide ? "modal wide" : "modal"} onMouseDown={e => e.stopPropagation()}><div className="modal-head"><h2>{title}</h2><div className="modal-head-actions">{action}<button className="icon-button" onClick={close}><X size={18} /></button></div></div>{children}</div></div>; }
function ModalActions({ close, busy, label }: { close: () => void; busy: boolean; label: string }) { return <div className="modal-actions"><button type="button" className="secondary" onClick={close}>Cancel</button><button className="primary" disabled={busy}>{busy && <LoaderCircle className="spin" size={16} />}{label}</button></div>; }
function CopyButton({ value, label }: { value: string; label?: string }) { const [copied, setCopied] = useState(false); return <button className={label ? "secondary" : "icon-button"} title="Copy" onClick={() => { void navigator.clipboard.writeText(value); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>{copied ? <Check size={16} /> : <Copy size={16} />}{label}</button>; }
function DeleteTarget({ id, refresh, success, fail }: Pick<Common, "refresh" | "success" | "fail"> & { id: string }) { return <button className="icon-button danger" onClick={async () => { if (!confirm("Delete this model target?")) return; try { await command("delete_target", { id }); await refresh(); success("Model target deleted"); } catch (e) { fail(e); } }}><Trash2 size={16} /></button>; }
function Toggle({ checked, onChange, label }: { checked: boolean; onChange: () => void; label?: string }) { return <button type="button" className={`toggle ${checked ? "on" : ""}`} role="switch" aria-checked={checked} aria-label={label} onClick={onChange}><i /></button>; }
function NumberSetting({ value: initial, suffix, onSave }: { value: string; suffix: string; onSave: (value: string) => void }) { const [value, setValue] = useState(initial); return <div className="number-setting"><input type="number" min="1" max="95" value={value} onChange={e => setValue(e.target.value)} onBlur={() => onSave(value)} /><span>{suffix}</span></div>; }
function ResourceNumber({ label, value, min, max, suffix, onChange, onSave }: { label: string; value: number; min: number; max: number; suffix: string; onChange: (value: number) => void; onSave: () => void }) { return <label className="resource-number"><span>{label}</span><div className="number-setting"><input aria-label={label} type="number" min={min} max={max} value={value} onChange={event => onChange(Math.max(min, Math.min(max, Number(event.target.value))))} onBlur={onSave} /><span>{suffix}</span></div></label>; }
function formatBytes(value: number | null) { if (!value) return "Size unknown"; const units = ["B", "KB", "MB", "GB", "TB"]; const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1); return `${(value / 1024 ** index).toFixed(index > 2 ? 1 : 0)} ${units[index]}`; }
function formatNumber(value: number) { return new Intl.NumberFormat().format(value); }
function formatToks(value?: number | null) { return value == null ? "—" : `${value.toFixed(1)} tok/s`; }
function formatUsd(value?: number | null) {
  if (value == null) return "Unknown";
  if (value === 0) return "$0.00";
  if (Math.abs(value) < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(2)}`;
}
function statusTone(status: number): "good" | "warn" | "bad" | "neutral" { if (status >= 200 && status < 300) return "good"; if (status >= 400 && status < 500) return "warn"; if (status >= 500) return "bad"; return "neutral"; }
function inflightPhase(request: InFlightRequest): string {
  const error = request.last_error_message || request.last_error_code;
  if (request.phase === "streaming") return "Streaming";
  if (request.phase === "retrying") return error ? `Retrying after ${error}` : "Retrying";
  if (request.phase === "rerouting") return error ? `Rerouting after ${error}` : "Rerouting";
  return "Trying";
}
function inflightDetail(request: InFlightRequest): string {
  const target = request.target_name ?? request.target_id ?? "Selecting model";
  const phase = inflightPhase(request);
  const attempt = request.attempt && request.attempt > 1 ? ` · attempt ${request.attempt}` : "";
  const error = request.phase !== "retrying" && request.phase !== "rerouting" && (request.last_error_message || request.last_error_code);
  return `${target} · ${phase}${attempt}${error ? ` · ${error}` : ""}`;
}
function recoveryLabel(attempts: number, status: number): string | null {
  if (attempts <= 1) return null;
  return status < 400 ? "rerouted" : "retried";
}
function routingAttemptLabel(attempt: { retry_after_until: string | null; transient_failure: boolean; reason: string }): string {
  if (attempt.retry_after_until) return `limited until ${new Date(attempt.retry_after_until).toLocaleTimeString()}`;
  if (attempt.reason.includes("same target")) return "retry";
  if (attempt.transient_failure) return "fallback";
  return "served";
}
function accessLabel(tier?: ProviderPreset["access_tier"]) { return ({ free_tier: "Free tier", starter_credits: "Starter credits", paid: "Paid", subscription: "Subscription", experimental: "Experimental" } as const)[tier ?? "paid"]; }
function protocolLabel(protocol: WireProtocol) { return ({ open_ai_chat: "OpenAI Chat", open_ai_responses: "OpenAI Responses", anthropic_messages: "Anthropic Messages", gemini_generate_content: "Gemini GenerateContent" } as const)[protocol]; }
