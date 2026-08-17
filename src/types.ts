export type TargetKind = "open_ai" | "open_router" | "gguf" | "mlx";

export interface Provider {
  id: string;
  name: string;
  kind: TargetKind;
  base_url: string;
  enabled: boolean;
  has_credential: boolean;
}

export interface ModelTarget {
  id: string;
  provider_id: string | null;
  name: string;
  kind: TargetKind;
  provider_model: string;
  local_path: string | null;
  runtime_url: string | null;
  capabilities: string[];
  enabled: boolean;
  state: string;
  size_bytes: number | null;
}

export interface RouteTarget {
  id: string;
  kind: TargetKind;
  model: string;
  priority: number;
  enabled: boolean;
}

export interface ModelRoute {
  alias: string;
  enabled: boolean;
  capabilities: string[];
  targets: RouteTarget[];
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
}

export interface DashboardData {
  running: boolean;
  base_url: string;
  provider_count: number;
  target_count: number;
  route_count: number;
  recent_requests: number;
  runtimes: Array<{ target_id: string; port: number; size_bytes: number; queued: number }>;
}

