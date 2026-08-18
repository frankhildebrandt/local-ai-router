export type TargetKind = "cloud" | "gguf" | "mlx" | "alias";
export type AuthMode = "api_key" | "open_ai_subscription";
export type WireProtocol = "open_ai_chat" | "open_ai_responses" | "anthropic_messages" | "gemini_generate_content";
export type AccessTier = "paid" | "subscription" | "free_tier" | "starter_credits" | "experimental";
export type ResourceProfile = "stealth" | "balanced" | "performance" | "custom";

export interface ResourceOverrides {
  memory_budget_mib?: number | null;
  auto_load?: boolean | null;
  idle_unload_minutes?: number | null;
  compute_duty_percent?: number | null;
  cpu_threads?: number | null;
  max_parallel_prompts?: number | null;
  process_priority?: number | null;
  gguf_gpu_layers?: number | null;
  disk_kv_enabled?: boolean | null;
}

export interface ResourcePolicy {
  version: 1;
  profile: ResourceProfile;
  memory_budget_percent: number;
  memory_budget_mib: number | null;
  auto_load: boolean;
  idle_unload_minutes: number;
  compute_duty_percent: number;
  cpu_threads: number;
  max_parallel_prompts: number;
  process_priority: number;
  gguf_gpu_layers: number;
  disk_kv_enabled: boolean;
  disk_kv_max_bytes: number;
}

export interface ProviderPreset {
  id: string;
  name: string;
  base_url: string | null;
  editable_base_url: boolean;
  auth_mode: AuthMode;
  auth_scheme: "bearer" | "x_api_key" | "x_goog_api_key" | "open_ai_subscription";
  default_protocol: WireProtocol;
  access_tier: AccessTier;
  access_type: "api_key" | "deployment" | "plan" | "subscription";
  discovery_strategy: "open_ai_models" | "gemini_models" | "curated";
  docs_url: string;
  note: string | null;
}

export interface Provider {
  id: string;
  name: string;
  preset_id: string;
  auth_mode: AuthMode;
  base_url: string;
  enabled: boolean;
  has_credential: boolean;
}

export interface ModelTarget {
  id: string;
  provider_id: string | null;
  name: string;
  kind: TargetKind;
  wire_protocol: WireProtocol;
  provider_model: string;
  local_path: string | null;
  runtime_url: string | null;
  capabilities: string[];
  enabled: boolean;
  state: string;
  size_bytes: number | null;
  task?: string | null;
  runtime_engine?: string | null;
  source_repo?: string | null;
  source_revision?: string | null;
  estimated_memory_bytes?: number | null;
  catalog_id?: string | null;
  trust_status?: string | null;
  resource_overrides?: ResourceOverrides | null;
}

export interface ProviderModel {
  id: string;
  wire_protocol: WireProtocol;
  capabilities: string[];
  context_window?: number | null;
  input_price_per_million?: number | null;
  output_price_per_million?: number | null;
}

export interface ModelMetadata {
  capabilities: string[];
  context_window: number;
  input_price_per_million: number | null;
  output_price_per_million: number | null;
  task_quality: Record<string, number>;
  source: "provider_api" | "catalog" | "fallback";
}

export type RouteRole = "primary" | "fallback";

export interface RouteTarget {
  id: string;
  kind: TargetKind;
  model: string;
  priority: number;
  enabled: boolean;
  role?: RouteRole;
}

export interface ModelRoute {
  alias: string;
  enabled: boolean;
  capabilities: string[];
  targets: RouteTarget[];
}

export interface PublicModel {
  id: string;
  source: "adaptive" | "target" | "alias";
  capabilities: string[];
}

export type RoutingPolicyStatus = "draft" | "shadow" | "active";
export type RoutingPrivacy = "local_only" | "local_preferred" | "cloud_allowed";

export interface RoutingWeights {
  quality: number;
  cost: number;
  latency: number;
  reliability: number;
  locality: number;
}

export interface TaskRule {
  id: string;
  task: string;
  priority: number;
  endpoint_contains: string | null;
  has_tools: boolean | null;
  modalities_any: string[];
  reasoning: boolean | null;
  min_input_tokens: number | null;
  max_input_tokens: number | null;
  text_pattern: string | null;
}

export interface RoutingPolicy {
  version: 1;
  alias: string;
  mode: "fixed" | "adaptive";
  status: RoutingPolicyStatus;
  privacy: RoutingPrivacy;
  default_task: string;
  weights: RoutingWeights;
  max_estimated_cost_usd: number | null;
  preferred_latency_ms: number;
  preferred_cost_usd: number;
  rules: TaskRule[];
  candidate_target_ids: string[];
}

export interface TargetRoutingProfile {
  version: 1;
  target_id: string;
  context_window: number;
  input_price_per_million: number | null;
  output_price_per_million: number | null;
  latency_prior_ms: number;
  reliability_prior: number;
  task_quality: Record<string, number>;
}

export interface RoutingTaskDefinition { id: string; label: string; builtin: boolean }
export interface ScoreComponents { quality: number; cost: number; latency: number; reliability: number; locality: number; total: number }
export interface RoutingEvaluation {
  alias: string;
  mode: "fixed" | "shadow" | "adaptive";
  task: string;
  task_source: "header" | "rule" | "default";
  task_rule_id?: string | null;
  decision: {
    task: string;
    ranked: Array<{ target_id: string; score: ScoreComponents; estimated_cost_usd: number | null; cost_verified: boolean }>;
    excluded: Array<{ target_id: string; reason: string }>;
  };
  ordered_target_ids: string[];
  shadow_target_id: string | null;
  half_open_target_ids: string[];
  estimated_input_tokens: number;
  peer_latency_ms?: number | null;
}

export interface RoutingAttempt {
  id: string; request_id: string; created_at: string; alias: string; task: string; task_source: string;
  target_id: string; routing_mode: string; status: number; transient_failure: boolean; retry_after_until: string | null; latency_ms: number;
  ttft_ms: number | null; streaming: boolean; estimated_cost_usd: number | null; cost_verified: boolean;
  input_tokens: number | null; output_tokens: number | null;
  score: ScoreComponents | null; reason: string;
}

export interface RoutingConfigExport {
  schema: "local-ai-router/routing-policy/v1";
  tasks: RoutingTaskDefinition[];
  profiles: TargetRoutingProfile[];
  policies: RoutingPolicy[];
}

export interface RequestLog {
  id: string;
  created_at: string;
  endpoint: string;
  alias: string | null;
  target: string | null;
  attempts: number;
  status: number;
  latency_ms: number;
  input_tokens: number | null;
  output_tokens: number | null;
  error_code: string | null;
  error_message: string | null;
  api_key_id: string | null;
  api_key_name: string | null;
}

export interface LocalApiKey {
  id: string;
  name: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface LocalApiKeyWithToken extends LocalApiKey {
  token: string;
}

export interface LogQuery {
  from?: string | null;
  to?: string | null;
  api_key_id?: string | null;
  legacy_only?: boolean;
  alias?: string | null;
  target?: string | null;
  endpoint?: string | null;
  status_class?: "success" | "4xx" | "5xx" | null;
  query?: string | null;
  limit?: number;
  offset?: number;
}

export interface LogResult {
  items: RequestLog[];
  total: number;
}

export interface LogFacets {
  aliases: string[];
  targets: string[];
  endpoints: string[];
}

export interface UsageSummary {
  request_count: number;
  success_count: number;
  average_latency_ms: number;
  input_tokens: number;
  output_tokens: number;
  unknown_usage_count: number;
}

export interface UsageData extends UsageSummary {
  buckets: Array<{ start: string; request_count: number; input_tokens: number; output_tokens: number }>;
  by_key: Array<UsageSummary & { api_key_id: string | null; api_key_name: string }>;
}

export interface ModelUsage extends UsageSummary {
  alias: string | null;
  target: string | null;
}

export interface KeyUsageData extends LocalApiKey, UsageSummary {
  buckets: Array<{ start: string; request_count: number; input_tokens: number; output_tokens: number }>;
  by_model: ModelUsage[];
}

export interface DashboardData {
  running: boolean;
  base_url: string;
  provider_count: number;
  target_count: number;
  route_count: number;
  recent_requests: number;
  inflight: InFlightRequest[];
  runtimes: Array<{ target_id: string; port: number; size_bytes: number; queued: number; active: number; resident_bytes: number; memory_warning: boolean; profile: ResourceProfile; compute_duty_percent: number; pending_restart: boolean }>;
}

export interface InFlightRequest {
  id: string;
  started_at: string;
  endpoint: string;
  alias: string;
  target_id: string | null;
  target_name: string | null;
  phase: "trying" | "streaming" | string;
}

export type CatalogCategory = "chat_vision" | "image" | "speech";
export type RamFit = "fits" | "tight" | "unsuitable";

export interface CatalogEntry {
  id: string;
  name: string;
  family: string;
  repo_id: string;
  category: CatalogCategory;
  task: string;
  runtime_engine: string;
  quantization: string;
  license: string;
  alias: string;
  capabilities: string[];
  download_bytes: number;
  estimated_memory_bytes: number;
  ram_fit: RamFit;
  trust_status: string;
  installable: boolean;
  lock_reason: string | null;
  voices: string[];
  gated: boolean;
  source?: string;
}

export interface LocalCatalog {
  platform: { apple_silicon: boolean; macos_15_plus: boolean; compatible: boolean; reason: string | null };
  memory_budget_bytes: number;
  memory_budget_percent: number;
  entries: CatalogEntry[];
}

export interface SearchPage {
  items: CatalogEntry[];
  next_cursor: string | null;
}

export interface ModelInspection {
  repo_id: string;
  revision: string;
  model_type: string | null;
  pipeline_tag: string | null;
  license: string | null;
  gated: boolean;
  mlx_format: boolean;
  download_bytes: number;
  files: string[];
  runtime_engine: string | null;
  task: string | null;
  category: CatalogCategory | null;
  capabilities: string[];
  estimated_memory_bytes: number;
  ram_fit: RamFit;
  installable: boolean;
  blockers: string[];
  trust_status: string;
}

export interface InstallJob {
  id: string;
  repo_id: string;
  revision: string;
  status: string;
  catalog_id: string | null;
  alias: string | null;
  engine: string | null;
  task: string | null;
  capabilities: string[];
  bytes_downloaded: number;
  bytes_total: number | null;
  current_file: string | null;
  staging_dir: string | null;
  error: string | null;
  confirm_over_budget: boolean;
  created_at: string;
  updated_at: string;
}

export interface InstallJobEvent {
  job_id: string;
  status: string;
  file: string | null;
  bytes_downloaded: number;
  bytes_total: number | null;
  progress: number;
}
