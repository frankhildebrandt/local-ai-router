import { QueryClient } from "@tanstack/react-query";
import { command } from "./api";
import type {
  DashboardData, InstallJob, KeyUsageData, LocalApiKey, LocalCatalog, LogFacets, LogQuery, LogResult,
  ModelMetadata, ModelRoute, ModelTarget, Provider, ProviderModel, ProviderPreset, PublicModel,
  ResourcePolicy, RoutingAttempt, RoutingConfigExport, RoutingPolicy, RoutingTaskDefinition,
  SearchPage, TargetRoutingProfile, UsageData,   AuthStatus, DirectoryGroup, DirectoryUser, NetworkModel, OidcAllowlistEntry, SharedImage,
  UplinkParent,
} from "./types";

export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        refetchOnWindowFocus: false,
        staleTime: 30_000,
      },
    },
  });
}

export const queryKeys = {
  dashboard: ["dashboard"] as const,
  providers: ["providers"] as const,
  providerPresets: ["provider-presets"] as const,
  targets: ["targets"] as const,
  routes: ["routes"] as const,
  publicModels: ["public-models"] as const,
  settings: ["settings"] as const,
  localKeys: ["local-api-keys"] as const,
  resourcePolicy: ["resource-policy"] as const,
  routingPolicies: ["routing-policies"] as const,
  routingProfiles: ["routing-profiles"] as const,
  routingTasks: ["routing-tasks"] as const,
  routingAttempts: ["routing-attempts"] as const,
  logFacets: ["log-facets"] as const,
  logs: (query: LogQuery) => ["logs", query] as const,
  usage: (period: string, target?: string | null) => ["usage", period, target ?? null] as const,
  keyUsage: (id: string, period: string) => ["key-usage", id, period] as const,
  catalog: ["local-catalog"] as const,
  installJobs: ["install-jobs"] as const,
  catalogSearch: (source: string, query: string) => ["catalog-search", source, query] as const,
  providerModels: (id: string) => ["provider-models", id] as const,
  modelMetadata: (model: string) => ["model-metadata", model] as const,
  routingConfig: ["routing-config"] as const,
  auth: ["auth"] as const,
  directoryUsers: ["directory-users"] as const,
  directoryGroups: ["directory-groups"] as const,
  oidcAllowlist: ["oidc-allowlist"] as const,
  uplink: ["uplink"] as const,
  networkModels: ["network-models"] as const,
  sharedImages: ["shared-images"] as const,
  parentSharedImages: ["parent-shared-images"] as const,
};

export function invalidateAppQueries(client: QueryClient) {
  return client.invalidateQueries({
    predicate: query => query.queryKey[0] !== "catalog-search",
  });
}

export const emptyDashboard: DashboardData = {
  running: false, base_url: "http://127.0.0.1:11435/v1", provider_count: 0, target_count: 0, route_count: 0,
  recent_requests: 0, inflight: [], runtimes: [],
};

export const defaultResourcePolicy: ResourcePolicy = {
  version: 1, profile: "stealth", memory_budget_percent: 50, memory_budget_mib: null, auto_load: true,
  idle_unload_minutes: 5, compute_duty_percent: 25,
  cpu_threads: Math.max(1, Math.floor((navigator.hardwareConcurrency || 4) / 2)),
  max_parallel_prompts: 1, process_priority: -1, gguf_gpu_layers: -1, disk_kv_enabled: true,
  disk_kv_max_bytes: 10 * 1024 ** 3,
};

export const emptyUsage: UsageData = {
  request_count: 0, success_count: 0, average_latency_ms: 0, input_tokens: 0, output_tokens: 0,
  cache_read_tokens: 0, cache_write_tokens: 0, unknown_usage_count: 0, tokens_per_second: null,
  estimated_cost_usd: null, buckets: [], by_key: [], by_model: [], throughput_candles: [], cost_candles: [],
};

export const emptyCatalog: LocalCatalog = {
  platform: { apple_silicon: true, macos_15_plus: true, compatible: true, reason: null },
  memory_budget_bytes: 0, memory_budget_percent: 70, entries: [],
};

export const fetchers = {
  dashboard: () => command<DashboardData>("dashboard"),
  providers: () => command<Provider[]>("list_providers"),
  providerPresets: () => command<ProviderPreset[]>("list_provider_presets"),
  targets: () => command<ModelTarget[]>("list_targets"),
  routes: () => command<ModelRoute[]>("list_routes"),
  publicModels: () => command<PublicModel[]>("list_public_models"),
  settings: () => command<Record<string, string>>("get_settings"),
  localKeys: () => command<LocalApiKey[]>("list_local_api_keys"),
  resourcePolicy: () => command<ResourcePolicy>("get_resource_policy"),
  routingPolicies: () => command<RoutingPolicy[]>("list_routing_policies"),
  routingProfiles: () => command<TargetRoutingProfile[]>("list_target_routing_profiles"),
  routingTasks: () => command<RoutingTaskDefinition[]>("list_routing_tasks"),
  routingAttempts: () => command<RoutingAttempt[]>("list_routing_attempts", { requestId: null, limit: 200 }),
  logFacets: () => command<LogFacets>("get_log_facets"),
  logs: (query: LogQuery) => command<LogResult>("list_logs", { query }),
  usage: (period: string, target?: string | null) => command<UsageData>("get_usage", target ? { period, target } : { period }),
  keyUsage: (id: string, period: string) => command<KeyUsageData>("get_key_usage", { id, period }),
  catalog: () => command<LocalCatalog>("list_local_catalog"),
  installJobs: () => command<InstallJob[]>("list_install_jobs"),
  catalogSearch: (query: string, source: string) => command<SearchPage>("search_mlx_catalog", { input: { query, cursor: null, source } }),
  providerModels: (id: string) => command<ProviderModel[]>("cached_provider_models", { id }),
  modelMetadata: (model: string) => command<ModelMetadata>("lookup_model_metadata", { model }),
  routingConfig: () => command<RoutingConfigExport>("export_routing_config"),
  auth: () => command<AuthStatus>("auth_status"),
  directoryUsers: () => command<DirectoryUser[]>("list_directory_users"),
  directoryGroups: () => command<DirectoryGroup[]>("list_directory_groups"),
  oidcAllowlist: () => command<OidcAllowlistEntry[]>("list_oidc_allowlist"),
  uplinkStatus: () => command<UplinkParent | null>("uplink_status"),
  networkModels: () => command<NetworkModel[]>("list_network_models"),
  sharedImages: () => command<SharedImage[]>("list_shared_images"),
  parentSharedImages: () => command<SharedImage[]>("list_parent_shared_images"),
};
