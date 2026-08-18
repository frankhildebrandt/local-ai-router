import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Box, Download, FileDown, LoaderCircle, Pause, Play, Search, Settings, Square, Trash2,
} from "lucide-react";
import { command, listenInstallJobs } from "./api";
import type {
  CatalogCategory, CatalogEntry, InstallJob, InstallJobEvent, LocalCatalog, ModelInspection,
  ModelTarget, RamFit, ResourceOverrides, ResourcePolicy, SearchPage, TargetKind,
} from "./types";
import { displayModelName, groupCatalogEntries, preferredQuantization } from "./catalogGroups";

type Common = { targets: ModelTarget[]; resourcePolicy: ResourcePolicy; refresh: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void };
type CatalogTab = "chat_vision" | "image" | "speech" | "library" | "import";
type DownloadSource = "huggingface" | "civitai" | "civitai.red";

const emptyCatalog: LocalCatalog = {
  platform: { apple_silicon: true, macos_15_plus: true, compatible: true, reason: null },
  memory_budget_bytes: 0,
  memory_budget_percent: 70,
  entries: [],
};

export function LocalPage({ targets, resourcePolicy, refresh, success, fail }: Common) {
  const local = targets.filter(target => target.kind === "gguf" || target.kind === "mlx");
  const [tab, setTab] = useState<CatalogTab>("chat_vision");
  const [catalog, setCatalog] = useState(emptyCatalog);
  const [jobs, setJobs] = useState<InstallJob[]>([]);
  const [query, setQuery] = useState("");
  const [family, setFamily] = useState("");
  const [task, setTask] = useState("");
  const [searchHits, setSearchHits] = useState<CatalogEntry[]>([]);
  const [searching, setSearching] = useState(false);
  const [downloadSource, setDownloadSource] = useState<DownloadSource>("huggingface");

  const load = useCallback(async () => {
    try {
      const [nextCatalog, nextJobs] = await Promise.all([
        command<LocalCatalog>("list_local_catalog"),
        command<InstallJob[]>("list_install_jobs"),
      ]);
      setCatalog(nextCatalog);
      setJobs(nextJobs);
    } catch (error) { fail(error); }
  }, [fail]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listenInstallJobs((event: InstallJobEvent) => {
      setJobs(current => current.map(job => job.id === event.job_id
        ? { ...job, status: event.status, current_file: event.file, bytes_downloaded: event.bytes_downloaded, bytes_total: event.bytes_total }
        : job));
      if (event.status === "completed") void Promise.all([load(), refresh()]);
    }).then(fn => { unlisten = fn; }).catch(() => undefined);
    return () => unlisten?.();
  }, [load, refresh]);

  useEffect(() => {
    if (!query.trim() || tab === "library" || tab === "import") { setSearchHits([]); return; }
    const timer = window.setTimeout(async () => {
      setSearching(true);
      try {
        const source = tab === "image" ? downloadSource : "huggingface";
        const page = await command<SearchPage>("search_mlx_catalog", { input: { query, cursor: null, source } });
        setSearchHits(page.items);
      } catch (error) { fail(error); }
      finally { setSearching(false); }
    }, 280);
    return () => window.clearTimeout(timer);
  }, [query, fail, tab, downloadSource]);

  const families = useMemo(() => Array.from(new Set(catalog.entries.map(item => item.family))), [catalog.entries]);
  const tasks = useMemo(() => Array.from(new Set(catalog.entries.map(item => item.task))), [catalog.entries]);
  const curated = groupCatalogEntries(catalog.entries.filter(item =>
    item.category === tab
    && (!family || item.family === family)
    && (!task || item.task === task)
    && (!query.trim() || `${item.name} ${item.repo_id} ${item.family}`.toLowerCase().includes(query.toLowerCase()))
  ));
  const visibleSearch = tab === "library" || tab === "import" ? [] : groupCatalogEntries(searchHits);

  const install = async (entry: CatalogEntry, inspected?: ModelInspection) => {
    if (entry.trust_status === "curated" && !entry.installable) {
      fail(entry.lock_reason ?? "This catalog entry cannot be installed");
      return;
    }
    if (entry.trust_status !== "curated") {
      const inspection = inspected ?? await command<ModelInspection>("inspect_mlx_model", { input: { repoId: entry.repo_id } });
      if (!inspection.installable) {
        fail(inspection.blockers[0] ?? "This untested model failed compatibility checks");
        return;
      }
    }
    if (entry.ram_fit !== "fits" && !confirm(`${entry.name} is ${entry.ram_fit === "tight" ? "tight" : "unsuitable"} for the current memory budget. Install anyway? Loading remains blocked if the budget is exceeded.`)) return;
    try {
      await command("install_catalog_model", { input: { repoId: entry.repo_id, catalogId: entry.trust_status === "curated" ? entry.id : null, confirmOverBudget: entry.ram_fit !== "fits", name: entry.name } });
      await load();
      success(`Installing ${entry.name}`);
    } catch (error) { fail(error); }
  };

  return <>
    <div className="page-head"><div><span className="eyebrow">On-device</span><h1>Local models</h1><p>Installed models are published immediately as public model IDs. Catalog, Hugging Face search, import and GGUF downloads stay available.</p></div></div>
    {!catalog.platform.compatible && <div className="security-note catalog-banner"><span>{catalog.platform.reason ?? "This Mac is not compatible with the MLX catalog."}</span></div>}
    <div className="segmented catalog-tabs">
      {([["chat_vision", "Chat & Vision"], ["image", "Image"], ["speech", "Speech"], ["library", "Library"], ["import", "Import"]] as const).map(([id, label]) =>
        <button key={id} className={tab === id ? "selected" : ""} onClick={() => { setTab(id); setQuery(""); }}>{label}</button>)}
    </div>
    {tab !== "library" && tab !== "import" && <>
      <section className="panel catalog-toolbar">
        <div className="search"><Search size={17} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder={tab === "image" && downloadSource !== "huggingface" ? "Search CivitAI checkpoints (SD and SDXL)" : "Search curated models or every Hugging Face MLX repository"} /></div>
        {tab === "image" && <div className="segmented small catalog-sources" role="group" aria-label="Download source">
          {([["huggingface", "Hugging Face"], ["civitai", "CivitAI"], ["civitai.red", "civitai.red"]] as const).map(([id, label]) =>
            <button key={id} type="button" className={downloadSource === id ? "selected" : ""} onClick={() => setDownloadSource(id)}>{label}</button>)}
        </div>}
        <div className="filter-grid catalog-filters">
          <select aria-label="Family" value={family} onChange={event => setFamily(event.target.value)}><option value="">All families</option>{families.map(item => <option key={item}>{item}</option>)}</select>
          <select aria-label="Task" value={task} onChange={event => setTask(event.target.value)}><option value="">All tasks</option>{tasks.map(item => <option key={item}>{item}</option>)}</select>
          <small>Budget {catalog.memory_budget_percent}% · {formatBytes(catalog.memory_budget_bytes)}</small>
        </div>
      </section>
      {!!jobs.length && <InstallJobs jobs={jobs} onChange={load} fail={fail} />}
      <div className="catalog-grid">{curated.map(group => <CatalogCard key={quantizationGroupKey(group)} variants={group} onInstall={entry => void install(entry)} />)}</div>
      {searching && <p className="catalog-hint">{tab === "image" && downloadSource !== "huggingface" ? `Searching ${downloadSource}…` : "Searching Hugging Face…"}</p>}
      {!!visibleSearch.length && <><h3 className="catalog-section">{tab === "image" && downloadSource !== "huggingface" ? `Untested ${downloadSource} matches` : "Untested Hugging Face matches"}</h3><p className="catalog-hint">{tab === "image" && downloadSource !== "huggingface" ? "SD and SDXL checkpoints download from the selected CivitAI host. Diffusers-layout models can generate immediately; classic single-file checkpoints are stored for the SD/SDXL engine." : "Visibility does not mean the model can be installed or run. Architecture, files and license are checked first."}</p><div className="catalog-grid">{visibleSearch.map(group => <CatalogCard key={quantizationGroupKey(group)} variants={group} onInstall={entry => void install(entry)} />)}</div></>}
      {!curated.length && !visibleSearch.length && !searching && <div className="empty"><div><Box /></div><h3>No models in this category</h3><p>Try another filter or search the Hugging Face MLX catalog.</p></div>}
    </>}
    {tab === "library" && <InstalledLibrary local={local} resourcePolicy={resourcePolicy} refresh={refresh} success={success} fail={fail} />}
    {tab === "import" && <ManualImport refresh={refresh} success={success} fail={fail} />}
  </>;
}

function quantizationGroupKey(group: CatalogEntry[]): string {
  return group.map(item => item.id).join(":");
}

function CatalogCard({ variants, onInstall }: { variants: CatalogEntry[]; onInstall: (entry: CatalogEntry) => void }) {
  const [selectedId, setSelectedId] = useState(() => preferredQuantization(variants).id);
  const entry = variants.find(item => item.id === selectedId) ?? preferredQuantization(variants);
  const ram = ramLabel(entry.ram_fit);
  return <article className="catalog-card">
    <div className="row"><h3>{displayModelName(entry)}</h3><span className={`badge ${ram.tone}`}>{ram.label}</span></div>
    <p>{entry.family} · {entry.license}</p>
    {variants.length > 1
      ? <label className="field"><span>Quantization</span><select aria-label="Quantization" value={entry.id} onChange={event => setSelectedId(event.target.value)}>{variants.map(item => <option value={item.id} key={item.id}>{item.quantization} · {formatBytes(item.download_bytes)} · {ramLabel(item.ram_fit).label}</option>)}</select></label>
      : <p>{entry.quantization} · {formatBytes(entry.download_bytes)} download · {formatBytes(entry.estimated_memory_bytes)} peak</p>}
    {variants.length > 1 && <p>{formatBytes(entry.download_bytes)} download · {formatBytes(entry.estimated_memory_bytes)} peak</p>}
    <div className="capabilities">{entry.capabilities.map(item => <span key={item}>{item}</span>)}<span>{entry.trust_status}</span></div>
    {!!entry.voices.length && <p>Voices: {entry.voices.join(", ")}</p>}
    {entry.lock_reason && <p className="catalog-lock">{entry.lock_reason}</p>}
    <button className="primary" disabled={entry.trust_status === "curated" && !entry.installable} onClick={() => onInstall(entry)}><Download size={15} />{entry.trust_status === "curated" && !entry.installable ? "Locked" : "Install"}</button>
  </article>;
}

function InstallJobs({ jobs, onChange, fail }: { jobs: InstallJob[]; onChange: () => Promise<void>; fail: (error: unknown) => void }) {
  const act = async (name: string, id: string) => { try { await command(name, { id }); await onChange(); } catch (error) { fail(error); } };
  return <section className="panel"><div className="panel-title"><div><h3>Installations</h3><p>Background downloads resume after restart.</p></div></div>
    {jobs.map(job => <div className="install-job" key={job.id}>
      <div className="row"><strong>{job.repo_id}</strong><span className={`badge ${job.status === "completed" ? "good" : job.status === "failed" ? "bad" : "neutral"}`}>{job.status}</span></div>
      <div className="progress"><i style={{ width: `${Math.round(((job.bytes_downloaded || 0) / Math.max(job.bytes_total || 1, 1)) * 100)}%` }} /></div>
      <small>{job.current_file ?? job.alias ?? job.revision} · {formatBytes(job.bytes_downloaded)} / {formatBytes(job.bytes_total)}</small>
      {job.error && <small className="catalog-lock">{job.error}</small>}
      <div className="button-row">
        {["downloading", "queued"].includes(job.status) && <button className="secondary" onClick={() => void act("pause_install_job", job.id)}><Pause size={14} />Pause</button>}
        {["paused", "interrupted", "failed"].includes(job.status) && <button className="secondary" onClick={() => void act("resume_install_job", job.id)}><Play size={14} />Resume</button>}
        {["downloading", "queued", "validating"].includes(job.status) && <button className="secondary" onClick={() => void act("cancel_install_job", job.id)}>Cancel</button>}
        {!["downloading", "queued", "validating"].includes(job.status) && <button className="secondary" onClick={() => void act("clear_install_job", job.id)}><Trash2 size={14} />Remove</button>}
      </div>
    </div>)}
  </section>;
}

function InstalledLibrary({ local, resourcePolicy, refresh, success, fail }: { local: ModelTarget[] } & Pick<Common, "resourcePolicy" | "refresh" | "success" | "fail">) {
  const [editing, setEditing] = useState<ModelTarget | null>(null);
  const toggle = async (model: ModelTarget) => { try { await command(model.state === "ready" ? "stop_local_model" : "start_local_model", { id: model.id }); await refresh(); success(model.state === "ready" ? "Model unloaded" : "Model loaded"); } catch (e) { fail(e); } };
  return <>
    <div className="model-grid">{local.map(model => <article className="model-card" key={model.id}><div className="model-icon"><Box /></div><div className="grow"><div className="row"><h3>{model.name}</h3><span className={`badge ${model.state === "ready" ? "good" : "neutral"}`}>{model.state}</span>{model.resource_overrides && <span className="badge neutral">Custom resources</span>}</div><p>{model.kind.toUpperCase()} · {formatBytes(model.size_bytes)}{model.provider_model ? ` · ${model.provider_model}` : ""}</p><div className="capabilities">{model.capabilities.map(item => <span key={item}>{item}</span>)}</div></div><button className="icon-button" title="Resource overrides" onClick={() => setEditing(model)}><Settings size={16} /></button><button className={model.state === "ready" ? "secondary" : "primary"} onClick={() => void toggle(model)}>{model.state === "ready" ? <Square size={15} /> : <Play size={15} />}{model.state === "ready" ? "Unload" : "Load"}</button><button className="icon-button danger" onClick={async () => { if (!confirm("Delete this model target?")) return; try { await command("delete_target", { id: model.id }); await refresh(); success("Model target deleted"); } catch (e) { fail(e); } }}><Trash2 size={16} /></button></article>)}</div>
    {!local.length && <div className="empty"><div><Box /></div><h3>Your local library is empty</h3><p>Install a catalog model, import a GGUF file, or download from Hugging Face.</p></div>}
    {editing && <ModelResourceEditor model={editing} global={resourcePolicy} close={() => setEditing(null)} done={async () => { setEditing(null); await refresh(); success("Model resource overrides saved; a loaded runtime restarts after active requests finish"); }} success={success} fail={fail} />}
  </>;
}

function ModelResourceEditor({ model, global, close, done, success, fail }: { model: ModelTarget; global: ResourcePolicy; close: () => void; done: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [values, setValues] = useState<ResourceOverrides>({ ...model.resource_overrides });
  const [busy, setBusy] = useState(false);
  const number = (key: keyof ResourceOverrides, fallback: number) => Number(values[key] ?? fallback);
  const save = async (overrides: ResourceOverrides | null) => { setBusy(true); try { await command("save_model_resource_overrides", { id: model.id, overrides }); await done(); } catch (error) { fail(error); } finally { setBusy(false); } };
  const updateParallel = (value: number) => setValues(current => ({ ...current, max_parallel_prompts: value, disk_kv_enabled: value === 1 ? current.disk_kv_enabled : false }));
  return <div className="modal-backdrop" onMouseDown={close}><div className="modal" onMouseDown={event => event.stopPropagation()}><div className="modal-head"><h2>{model.name} resources</h2><button className="icon-button" onClick={close}>×</button></div><div className="form resource-override-form">
    <label className="field"><span>Compute duty %</span><input type="number" min="5" max="100" value={number("compute_duty_percent", global.compute_duty_percent)} onChange={event => setValues({ ...values, compute_duty_percent: Number(event.target.value) })} /></label>
    <label className="field"><span>CPU threads</span><input type="number" min="1" max="128" value={number("cpu_threads", global.cpu_threads)} onChange={event => setValues({ ...values, cpu_threads: Number(event.target.value) })} /></label>
    <label className="field"><span>Process priority (-1 = background)</span><input type="number" min="-1" max="2" value={number("process_priority", global.process_priority)} onChange={event => setValues({ ...values, process_priority: Number(event.target.value) })} /></label>
    <label className="field"><span>Parallel prompts</span><input type="number" min="1" max="16" value={number("max_parallel_prompts", global.max_parallel_prompts)} onChange={event => updateParallel(Number(event.target.value))} /></label>
    <label className="field"><span>Idle unload minutes</span><input type="number" min="0" max="1440" value={number("idle_unload_minutes", global.idle_unload_minutes)} onChange={event => setValues({ ...values, idle_unload_minutes: Number(event.target.value) })} /></label>
    <label className="field"><span>Memory cap MiB (0 = global)</span><input type="number" min="0" value={number("memory_budget_mib", 0)} onChange={event => setValues({ ...values, memory_budget_mib: Number(event.target.value) || null })} /></label>
    {model.kind === "gguf" && <label className="field"><span>GPU layers (-1 = auto)</span><input type="number" min="-1" max="999" value={number("gguf_gpu_layers", global.gguf_gpu_layers)} onChange={event => setValues({ ...values, gguf_gpu_layers: Number(event.target.value) })} /></label>}
    <label className="field"><span>Automatic load</span><input type="checkbox" checked={values.auto_load ?? global.auto_load} onChange={event => setValues({ ...values, auto_load: event.target.checked })} /></label>
    {model.kind === "gguf" && <label className="field"><span>Persistent KV (requires parallel = 1)</span><input type="checkbox" checked={values.disk_kv_enabled ?? global.disk_kv_enabled} onChange={event => setValues({ ...values, disk_kv_enabled: event.target.checked, max_parallel_prompts: event.target.checked ? 1 : values.max_parallel_prompts })} /></label>}
    {model.kind === "mlx" && <p className="catalog-hint">Persistent disk KV is not supported by the MLX runtime. MLX continues to use Metal; Apple Neural Engine quotas are unavailable.</p>}
    <div className="modal-actions">{model.kind === "gguf" && <button className="secondary danger-text" disabled={busy} onClick={async () => { if (!confirm(`Delete persistent KV snapshots for “${model.name}”?`)) return; try { await command("clear_kv_cache", { targetId: model.id }); success("Model KV snapshots deleted"); } catch (error) { fail(error); } }}><Trash2 size={15} />Clear KV cache</button>}<button className="secondary" disabled={busy} onClick={() => void save(null)}>Use global profile</button><button className="primary" disabled={busy} onClick={() => void save(values)}>{busy && <LoaderCircle className="spin" size={15} />}Save overrides</button></div>
  </div></div></div>;
}

function ManualImport({ refresh, success, fail }: Pick<Common, "refresh" | "success" | "fail">) {
  const [tab, setTab] = useState<"import" | "download">("import");
  const [kind, setKind] = useState<TargetKind>("gguf");
  const [source, setSource] = useState("");
  const [filename, setFilename] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [downloadSource, setDownloadSource] = useState<DownloadSource>("huggingface");
  const civitai = tab === "download" && downloadSource !== "huggingface";
  const browse = async () => { const result = await open({ directory: kind === "mlx", multiple: false, filters: kind === "gguf" ? [{ name: "GGUF model", extensions: ["gguf"] }] : undefined }); if (typeof result === "string") { setSource(result); setName(result.split("/").pop() ?? "Local model"); } };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      if (tab === "import") {
        await command("import_local_model", { input: { source, name, kind, aliasModel: name, capabilities: ["chat"] } });
      } else if (civitai) {
        const repoId = source.includes("civitai") ? source : `${downloadSource}/models/${source.replace(/^models\//, "")}`;
        await command("install_catalog_model", { input: { repoId, catalogId: null, confirmOverBudget: false, name: name || repoId } });
      } else {
        await command("download_local_model", { input: { repoId: source, filename: filename || null, name: name || source.split("/").pop(), kind, aliasModel: name || source, capabilities: ["chat"], source: downloadSource } });
      }
      setSource(""); setName(""); setFilename("");
      await refresh();
      success(civitai ? "CivitAI download queued" : "Local model added to the library");
    } catch (e) { fail(e); } finally { setBusy(false); }
  };
  return <section className="panel add-model"><div className="segmented small"><button className={tab === "import" ? "selected" : ""} onClick={() => setTab("import")}><FileDown size={15} />Import</button><button className={tab === "download" ? "selected" : ""} onClick={() => setTab("download")}><Download size={15} />Download</button></div>
    {tab === "download" && <div className="segmented small catalog-sources" role="group" aria-label="Download source">{([["huggingface", "Hugging Face"], ["civitai", "CivitAI"], ["civitai.red", "civitai.red"]] as const).map(([id, label]) => <button key={id} type="button" className={downloadSource === id ? "selected" : ""} onClick={() => { setDownloadSource(id); if (id !== "huggingface") setKind("mlx"); }}>{label}</button>)}</div>}
    <form onSubmit={submit}>{!civitai && <select value={kind} onChange={e => setKind(e.target.value as TargetKind)}><option value="gguf">GGUF · llama.cpp</option><option value="mlx">MLX · Apple Silicon</option></select>}<div className="input-action"><input value={source} onChange={e => setSource(e.target.value)} placeholder={tab === "import" ? "Model file or folder" : civitai ? "https://civitai.com/models/… or models/12345" : "org/model-repository"} required />{tab === "import" && <button type="button" className="secondary" onClick={() => void browse()}>Browse</button>}</div>{tab === "download" && kind === "gguf" && !civitai && <input value={filename} onChange={e => setFilename(e.target.value)} placeholder="quantized-model.Q4_K_M.gguf" required />}<input value={name} onChange={e => setName(e.target.value)} placeholder="Display name" required={tab === "import"} /><button className="primary" disabled={busy}>{busy ? <LoaderCircle className="spin" size={17} /> : tab === "import" ? <FileDown size={17} /> : <Download size={17} />}{tab === "import" ? "Import" : "Download"}</button></form></section>;
}

function ramLabel(fit: RamFit): { label: string; tone: "good" | "warn" | "bad" } {
  if (fit === "fits") return { label: "Fits", tone: "good" };
  if (fit === "tight") return { label: "Tight", tone: "warn" };
  return { label: "Unsuitable", tone: "bad" };
}

function formatBytes(value: number | null) {
  if (!value) return "Size unknown";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / 1024 ** index).toFixed(index > 2 ? 1 : 0)} ${units[index]}`;
}

export type { CatalogCategory };
