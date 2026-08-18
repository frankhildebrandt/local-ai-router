import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

const command = vi.fn();
vi.mock("./api", () => ({ command: (...args: unknown[]) => command(...args), appVersion: async () => "0.2.1", listenInstallJobs: async () => () => undefined, listenDesktopNavigate: async () => () => undefined, listenGatewayTraffic: async () => () => undefined, errorMessage: (error: unknown) => String(error), isTauri: () => true }));

beforeEach(() => {
  command.mockImplementation((name: string, args?: unknown) => {
    if (name === "dashboard") return Promise.resolve({ running: true, base_url: "http://127.0.0.1:11435/v1", provider_count: 0, target_count: 0, route_count: 0, recent_requests: 1, inflight: [], runtimes: [] });
    if (name === "list_providers") return Promise.resolve([{ id: "openai", name: "OpenAI", preset_id: "openai", auth_mode: "api_key", base_url: "https://api.openai.com/v1", enabled: true, has_credential: true }]);
    if (name === "list_targets") return Promise.resolve([{ id: "cloud", provider_id: "openai", name: "Coding model", kind: "cloud", wire_protocol: "open_ai_chat", provider_model: "coding", local_path: null, runtime_url: null, capabilities: ["chat", "streaming"], enabled: true, state: "ready", size_bytes: null }]);
    if (name === "list_routes") return Promise.resolve([{ alias: "assistant", enabled: true, capabilities: ["chat", "streaming"], targets: [{ id: "cloud", kind: "cloud", model: "coding", priority: 10, enabled: true }] }]);
    if (name === "list_public_models") return Promise.resolve([
      { id: "adaptive-routing", source: "adaptive", capabilities: ["chat", "streaming"] },
      { id: "coding", source: "target", capabilities: ["chat", "streaming"] },
      { id: "assistant", source: "alias", capabilities: ["chat", "streaming"] },
    ]);
    if (name === "list_routing_policies" || name === "list_target_routing_profiles") return Promise.resolve([]);
    if (name === "lookup_model_metadata") return Promise.resolve({ capabilities: ["chat", "streaming"], context_window: 8192, input_price_per_million: null, output_price_per_million: null, task_quality: { general: 50 }, source: "fallback" });
    if (name === "list_routing_attempts") return Promise.resolve([]);
    if (name === "list_routing_tasks") return Promise.resolve([{ id: "general", label: "general", builtin: true }, { id: "coding", label: "coding", builtin: true }]);
    if (name === "simulate_routing") {
      const policy = (args as { input?: { policy?: { mode?: string; status?: string } } } | undefined)?.input?.policy;
      const mode = policy?.mode === "adaptive" && policy.status === "active" ? "adaptive" : policy?.mode === "adaptive" && policy.status === "shadow" ? "shadow" : "fixed";
      return Promise.resolve({ alias: "assistant", mode, task: "coding", task_source: "header", ordered_target_ids: ["cloud"], shadow_target_id: null, estimated_input_tokens: 3, decision: { task: "coding", ranked: [{ target_id: "cloud", score: { quality: .9, cost: .5, latency: .5, reliability: .95, locality: 0, total: .8 }, estimated_cost_usd: null, cost_verified: false }], excluded: [] } });
    }
    if (name === "export_routing_config") return Promise.resolve({ schema: "local-ai-router/routing-policy/v1", tasks: [], profiles: [], policies: [] });
    if (name === "list_provider_presets") return Promise.resolve([]);
    if (name === "list_local_api_keys") return Promise.resolve([{ id: "default", name: "Default", created_at: "2026-08-17T10:00:00Z", last_used_at: null, revoked_at: null }]);
    if (name === "list_logs") return Promise.resolve({ total: 1, items: [{ id: "request", created_at: "2026-08-17T10:00:00Z", endpoint: "/v1/chat/completions", alias: "assistant", target: "cloud", attempts: 1, status: 200, latency_ms: 12, input_tokens: 3, output_tokens: 5, error_code: null, error_message: null, api_key_id: "default", api_key_name: "Default" }] });
    if (name === "get_settings") return Promise.resolve({});
    if (name === "get_resource_policy") return Promise.resolve({ version: 1, profile: "stealth", memory_budget_percent: 50, memory_budget_mib: null, auto_load: true, idle_unload_minutes: 5, compute_duty_percent: 25, cpu_threads: 4, max_parallel_prompts: 1, process_priority: -1, gguf_gpu_layers: -1, disk_kv_enabled: true, disk_kv_max_bytes: 10 * 1024 ** 3 });
    if (name === "get_resource_profile_preset") return Promise.resolve({ version: 1, profile: "balanced", memory_budget_percent: 70, memory_budget_mib: null, auto_load: true, idle_unload_minutes: 15, compute_duty_percent: 60, cpu_threads: 8, max_parallel_prompts: 2, process_priority: 0, gguf_gpu_layers: -1, disk_kv_enabled: false, disk_kv_max_bytes: 10 * 1024 ** 3 });
    if (name === "get_log_facets") return Promise.resolve({ aliases: ["assistant"], targets: ["cloud"], endpoints: ["/v1/chat/completions"] });
    if (name === "get_usage") return Promise.resolve({ request_count: 17, success_count: 16, average_latency_ms: 12, input_tokens: 128, output_tokens: 256, cache_read_tokens: 16, cache_write_tokens: 0, unknown_usage_count: 0, tokens_per_second: 21.3, estimated_cost_usd: 0.012, buckets: [{ start: "2026-08-17T10:00:00Z", request_count: 17, input_tokens: 128, output_tokens: 256 }], by_key: [{ api_key_id: "default", api_key_name: "Default", request_count: 17, success_count: 16, average_latency_ms: 12, input_tokens: 128, output_tokens: 256, unknown_usage_count: 0 }], by_model: [{ alias: "assistant", target: "coding", request_count: 17, success_count: 16, average_latency_ms: 12, input_tokens: 32, output_tokens: 64, unknown_usage_count: 0, tokens_per_second: 21.3, estimated_cost_usd: 0.012 }], throughput_candles: [{ start: "2026-08-17T10:00:00Z", open: 18, high: 24, low: 12, close: 21.3, avg: 20 }], cost_candles: [{ start: "2026-08-17T10:00:00Z", open: 0.01, high: 0.02, low: 0.008, close: 0.012, avg: 0.012 }] });
    if (name === "create_local_api_key") return Promise.resolve({ id: "new-key", name: "Automation", created_at: "2026-08-17T11:00:00Z", last_used_at: null, revoked_at: null, token: "lar_new" });
    if (name === "get_key_usage") return Promise.resolve({
      id: "default", name: "Default", created_at: "2026-08-17T10:00:00Z", last_used_at: null, revoked_at: null,
      request_count: 17, success_count: 16, average_latency_ms: 12, input_tokens: 128, output_tokens: 256, unknown_usage_count: 0,
      buckets: [{ start: "2026-08-17T10:00:00Z", request_count: 17, input_tokens: 128, output_tokens: 256 }],
      by_model: [{ alias: "assistant", target: "coding", request_count: 17, success_count: 16, average_latency_ms: 12, input_tokens: 32, output_tokens: 64, unknown_usage_count: 0 }],
    });
    if (name === "client_chat") return Promise.resolve({ model: "assistant", content: "Hello from the router" });
    if (name === "list_local_catalog") return Promise.resolve({
      platform: { apple_silicon: true, macos_15_plus: true, compatible: true, reason: null },
      memory_budget_bytes: 24 * 1024 * 1024 * 1024,
      memory_budget_percent: 70,
      entries: [
        { id: "qwen-3-5-4b", name: "Qwen 3.5 4B 4-bit", family: "Qwen 3.5", repo_id: "mlx-community/Qwen3.5-4B-MLX-4bit", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "4-bit", license: "apache-2.0", alias: "qwen-3-5-4b", capabilities: ["chat", "streaming"], download_bytes: 3_000_000_000, estimated_memory_bytes: 4_000_000_000, ram_fit: "fits", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "qwen-3-5-4b-8bit", name: "Qwen 3.5 4B 8-bit", family: "Qwen 3.5", repo_id: "mlx-community/Qwen3.5-4B-MLX-8bit", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "8-bit", license: "apache-2.0", alias: "qwen-3-5-4b-8bit", capabilities: ["chat", "streaming"], download_bytes: 5_000_000_000, estimated_memory_bytes: 7_000_000_000, ram_fit: "tight", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "gemma-4-audio", name: "Gemma 4 Audio", family: "Gemma 4", repo_id: "mlx-community/gemma-4-audio", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "4-bit", license: "gemma", alias: "gemma-4-audio", capabilities: ["chat", "audio_input"], download_bytes: 4_000_000_000, estimated_memory_bytes: 6_000_000_000, ram_fit: "tight", trust_status: "curated", installable: false, lock_reason: "Audio input is locked until upstream support is stable", voices: [], gated: false },
        { id: "flux2-klein", name: "FLUX.2 Klein 4B", family: "FLUX.2", repo_id: "mlx-community/FLUX.2-klein-4B-4bit", category: "image", task: "image", runtime_engine: "mlx_image", quantization: "4-bit", license: "flux-1-dev", alias: "flux2-klein", capabilities: ["images"], download_bytes: 8_000_000_000, estimated_memory_bytes: 16_000_000_000, ram_fit: "unsuitable", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "sdxl-turbo", name: "SDXL Turbo", family: "Stable Diffusion", repo_id: "mlx-community/sdxl-turbo", category: "image", task: "image", runtime_engine: "mlx_image", quantization: "fp16", license: "openrail++", alias: "sdxl-turbo", capabilities: ["images"], download_bytes: 7_000_000_000, estimated_memory_bytes: 8_000_000_000, ram_fit: "tight", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "sd-2-1-base", name: "Stable Diffusion 2.1 Base", family: "Stable Diffusion", repo_id: "stabilityai/stable-diffusion-2-1-base", category: "image", task: "image", runtime_engine: "mlx_image", quantization: "fp16", license: "openrail++", alias: "sd-2-1-base", capabilities: ["images"], download_bytes: 5_000_000_000, estimated_memory_bytes: 7_000_000_000, ram_fit: "tight", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "kokoro", name: "Kokoro 82M", family: "Kokoro", repo_id: "mweinbach/Kokoro-82M-Swift", category: "speech", task: "speech", runtime_engine: "mlx_speech", quantization: "fp16", license: "apache-2.0", alias: "kokoro", capabilities: ["speech"], download_bytes: 350_000_000, estimated_memory_bytes: 500_000_000, ram_fit: "fits", trust_status: "curated", installable: true, lock_reason: null, voices: ["af_heart", "af_bella"], gated: false },
      ],
    });
    if (name === "list_install_jobs") return Promise.resolve([{ id: "job-1", repo_id: "mlx-community/Qwen3.5-2B-4bit", revision: "abc", status: "downloading", catalog_id: "qwen-3-5-2b", alias: "qwen-3-5-2b", engine: "mlx_chat", task: "chat", capabilities: ["chat"], bytes_downloaded: 500, bytes_total: 1000, current_file: "model.safetensors", staging_dir: "/tmp", error: null, confirm_over_budget: false, created_at: "2026-08-17T10:00:00Z", updated_at: "2026-08-17T10:00:00Z" }]);
    if (name === "search_mlx_catalog") {
      const source = (args as { input?: { source?: string } } | undefined)?.input?.source;
      if (source === "civitai") {
        return Promise.resolve({ items: [{ id: "civitai/models/4201@130072", name: "Realistic Vision", family: "SD 1.5", repo_id: "civitai/models/4201@130072", category: "image", task: "image", runtime_engine: "mlx_image", quantization: "fp16", license: "civitai", alias: "realistic-vision", capabilities: ["images"], download_bytes: 2_000_000_000, estimated_memory_bytes: 4_000_000_000, ram_fit: "fits", trust_status: "untested", installable: true, lock_reason: null, voices: [], gated: false, source: "civitai" }], next_cursor: null });
      }
      return Promise.resolve({ items: [{ id: "org/untested", name: "Untested MLX", family: "Unknown", repo_id: "org/untested", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "unknown", license: "unknown", alias: "untested", capabilities: ["chat"], download_bytes: 1, estimated_memory_bytes: 2, ram_fit: "fits", trust_status: "untested", installable: false, lock_reason: "unknown architecture", voices: [], gated: false }], next_cursor: null });
    }
    if (name === "inspect_mlx_model") return Promise.resolve({ repo_id: "org/untested", revision: "main", model_type: "mystery", pipeline_tag: "text-generation", license: "mit", gated: false, mlx_format: true, download_bytes: 1, files: ["config.json"], runtime_engine: null, task: null, category: "chat_vision", capabilities: [], estimated_memory_bytes: 2, ram_fit: "fits", installable: false, blockers: ["unknown architecture"], trust_status: "untested" });
    if (name === "install_catalog_model") return Promise.resolve({ id: "job-2", repo_id: "mlx-community/Qwen3.5-4B-MLX-4bit", revision: "abc", status: "queued", catalog_id: "qwen-3-5-4b", alias: null, engine: "mlx_chat", task: "chat", capabilities: ["chat"], bytes_downloaded: 0, bytes_total: 1, current_file: null, staging_dir: null, error: null, confirm_over_budget: false, created_at: "2026-08-17T10:00:00Z", updated_at: "2026-08-17T10:00:00Z" });
    return Promise.resolve(null);
  });
});
afterEach(cleanup);

describe("Local AI Router shell", () => {
  it("opens the preconfigured chat and sends through the selected model", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    expect(await screen.findByText("Start a private conversation")).toBeInTheDocument();
    expect(screen.getByLabelText("Model")).toHaveValue("adaptive-routing");
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("client_chat", expect.objectContaining({ input: expect.objectContaining({ model: "adaptive-routing" }) })));
    expect(await screen.findByText("Hello from the router")).toBeInTheDocument();
  });

  it("shows the private gateway quickstart without requiring a backend", async () => {
    render(<App />);
    expect(await screen.findByText("Your models, one local endpoint.")).toBeTruthy();
    expect(screen.getAllByText("http://127.0.0.1:11435/v1").length).toBeGreaterThan(0);
    expect(screen.getByRole("heading", { name: "Active requests" })).toBeInTheDocument();
    expect(screen.getByText("No running requests")).toBeInTheDocument();
    expect(command).toHaveBeenCalledWith("list_logs", { query: { legacy_only: false, limit: 100 } });
  });

  it("lets the user stop in-flight requests from overview", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "dashboard") {
        return Promise.resolve({
          running: true,
          base_url: "http://127.0.0.1:11435/v1",
          provider_count: 0,
          target_count: 0,
          route_count: 0,
          recent_requests: 1,
          inflight: [
            {
              id: "req-1",
              started_at: "2026-08-18T10:00:00Z",
              endpoint: "/v1/chat/completions",
              alias: "assistant",
              target_id: "cloud",
              target_name: "Coding model",
              phase: "streaming",
            },
            {
              id: "req-2",
              started_at: "2026-08-18T10:00:01Z",
              endpoint: "/v1/responses",
              alias: "coder",
              target_id: "local",
              target_name: "Local model",
              phase: "trying",
            },
          ],
          runtimes: [],
        });
      }
      return previous?.(name, args);
    });
    render(<App />);
    expect(await screen.findByText("assistant")).toBeInTheDocument();
    expect(screen.getByText("coder")).toBeInTheDocument();
    expect(screen.getByText("2 in flight")).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Stop request" })[0]);
    await waitFor(() => expect(command).toHaveBeenCalledWith("cancel_inflight_request", { id: "req-1" }));
    fireEvent.click(screen.getByRole("button", { name: "Stop all" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("cancel_all_inflight_requests"));
  });

  it("shows loaded local runtimes with token throughput on overview", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "list_targets") {
        return Promise.resolve([
          { id: "local-qwen", provider_id: null, name: "Qwen 3.5", kind: "mlx", wire_protocol: "open_ai_chat", provider_model: "qwen", local_path: "/models/qwen", runtime_url: "http://127.0.0.1:12100/v1", capabilities: ["chat", "streaming"], enabled: true, state: "ready", size_bytes: 4_000_000_000 },
          { id: "installed-only", provider_id: null, name: "Idle GGUF", kind: "gguf", wire_protocol: "open_ai_chat", provider_model: "idle", local_path: "/models/idle", runtime_url: null, capabilities: ["chat"], enabled: true, state: "stopped", size_bytes: 2_000_000_000 },
        ]);
      }
      if (name === "dashboard") {
        return Promise.resolve({
          running: true,
          base_url: "http://127.0.0.1:11435/v1",
          provider_count: 0,
          target_count: 2,
          route_count: 0,
          recent_requests: 1,
          inflight: [],
          runtimes: [{ target_id: "local-qwen", port: 12100, size_bytes: 4_000_000_000, queued: 0, active: 1, resident_bytes: 3_000_000_000, memory_warning: false, profile: "balanced", compute_duty_percent: 60, pending_restart: false, tokens_per_second: 12.4 }],
        });
      }
      return previous?.(name, args);
    });
    render(<App />);
    expect(await screen.findByText("Qwen 3.5")).toBeInTheDocument();
    expect(screen.getByText(/12.4 tok\/s/)).toBeInTheDocument();
    expect(screen.getByText("1 loaded · 2 installed")).toBeInTheDocument();
    expect(screen.queryByText("Idle GGUF")).not.toBeInTheDocument();
    expect(screen.queryByText("No models loaded")).not.toBeInTheDocument();
  });

  it("keeps window chrome draggable without capturing toolbar buttons", async () => {
    const { container } = render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    expect(container.querySelector(".sidebar-drag")).toHaveAttribute("data-tauri-drag-region");
    expect(container.querySelector(".brand")).toHaveAttribute("data-tauri-drag-region");
    expect(container.querySelector(".crumb")).toHaveAttribute("data-tauri-drag-region");
    for (const button of container.querySelectorAll(".topbar button")) {
      expect(button).not.toHaveAttribute("data-tauri-drag-region");
    }
  });

  it("exposes usage, named keys, and structured request-log filters", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");

    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    expect(await screen.findByText("Usage by API key")).toBeInTheDocument();
    expect(screen.getByText("Current tokens/s")).toBeInTheDocument();
    expect(screen.getByText("Theoretical cost")).toBeInTheDocument();
    expect(screen.getByText("Usage by model")).toBeInTheDocument();
    expect(screen.getByText("Input tokens")).toBeInTheDocument();
    expect(screen.getByText("Output tokens")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "24h" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("get_usage", { period: "24h" }));
    fireEvent.click(screen.getByRole("button", { name: /coding/ }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("get_usage", { period: "24h", target: "coding" }));

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByLabelText("Inference profile")).toHaveValue("stealth");
    expect(screen.getByRole("heading", { name: "Persistent local KV" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Local API keys" })).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText("New key name")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Inference profile"), { target: { value: "balanced" } });
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_resource_policy", expect.objectContaining({ policy: expect.objectContaining({ profile: "balanced", compute_duty_percent: 60, max_parallel_prompts: 2 }) })));

    fireEvent.click(screen.getByRole("button", { name: "API keys" }));
    expect(await screen.findByRole("heading", { name: "API keys" })).toBeInTheDocument();
    expect(screen.getByText("Default")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("New key name"), { target: { value: "Automation" } });
    fireEvent.click(screen.getByRole("button", { name: "Create key" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("create_local_api_key", { name: "Automation" }));

    fireEvent.click(screen.getByRole("button", { name: "Request logs" }));
    expect(await screen.findByLabelText("API key")).toBeInTheDocument();
    expect(screen.getByLabelText("Status")).toBeInTheDocument();
    expect(await screen.findByText("1 matching request")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Adaptive routing" })).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Status"), { target: { value: "5xx" } });
    await waitFor(() => expect(command).toHaveBeenCalledWith("list_logs", expect.objectContaining({ query: expect.objectContaining({ status_class: "5xx" }) })));

    fireEvent.click(screen.getByRole("button", { name: "Routing" }));
    expect(await screen.findByRole("heading", { name: "Routing" })).toBeInTheDocument();
    expect(screen.getByText("No routing attempts")).toBeInTheDocument();
    await waitFor(() => expect(command).toHaveBeenCalledWith("list_routing_attempts", { requestId: null, limit: 200 }));
  });

  it("shows request and token usage on each local API key", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "API keys" }));
    expect(await screen.findByRole("heading", { name: "API keys" })).toBeInTheDocument();
    await waitFor(() => expect(command).toHaveBeenCalledWith("get_usage", { period: "7d" }));
    const row = screen.getByRole("button", { name: /Default/ });
    expect(row).toHaveTextContent("17");
    expect(row).toHaveTextContent("128");
    expect(row).toHaveTextContent("256");
  });

  it("opens key details with per-model token usage", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "API keys" }));
    fireEvent.click(await screen.findByRole("button", { name: /Default/ }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("get_key_usage", { id: "default", period: "7d" }));
    expect(await screen.findByRole("heading", { name: "Default" })).toBeInTheDocument();
    expect(screen.getByText("assistant")).toBeInTheDocument();
    expect(screen.getByText("coding")).toBeInTheDocument();
    expect(screen.getByText("32")).toBeInTheDocument();
    expect(screen.getByText("64")).toBeInTheDocument();
  });

  it("shows curated catalog categories, RAM badges, locked models, and import paths", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Local models" }));
    expect(await screen.findByText("Qwen 3.5 4B")).toBeInTheDocument();
    expect(screen.getByText("Fits")).toBeInTheDocument();
    expect(screen.getAllByText("curated").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Chat & Vision" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Qwen 3.5 4B" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Qwen 3.5 4B 8-bit" })).not.toBeInTheDocument();
    const quant = screen.getByLabelText("Quantization");
    expect(quant).toHaveValue("qwen-3-5-4b");
    expect(quant).toHaveTextContent("4-bit");
    expect(quant).toHaveTextContent("8-bit");
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("install_catalog_model", expect.objectContaining({ input: expect.objectContaining({ repoId: "mlx-community/Qwen3.5-4B-MLX-4bit" }) })));
    fireEvent.change(quant, { target: { value: "qwen-3-5-4b-8bit" } });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("install_catalog_model", expect.objectContaining({ input: expect.objectContaining({ repoId: "mlx-community/Qwen3.5-4B-MLX-8bit" }) })));
    expect(screen.getByText("Gemma 4 Audio")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Locked" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Image" }));
    expect(await screen.findByText("FLUX.2 Klein 4B")).toBeInTheDocument();
    expect(screen.getByText("SDXL Turbo")).toBeInTheDocument();
    expect(screen.getByText("Stable Diffusion 2.1 Base")).toBeInTheDocument();
    expect(screen.getByText("Unsuitable")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "CivitAI" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "civitai.red" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "CivitAI" }));
    fireEvent.change(screen.getByPlaceholderText("Search CivitAI checkpoints (SD and SDXL)"), { target: { value: "realistic" } });
    expect(await screen.findByText("Realistic Vision")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Speech" }));
    expect(await screen.findByText("Kokoro 82M")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("Search curated models or every Hugging Face MLX repository"), { target: { value: "mystery" } });
    expect(await screen.findByText("Untested Hugging Face matches")).toBeInTheDocument();
    expect(screen.getByText("Untested MLX")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    expect(screen.getByRole("button", { name: "Download" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(screen.getByRole("button", { name: "Hugging Face" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "CivitAI" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "civitai.red" })).not.toBeInTheDocument();
    expect(screen.getByText("GGUF · llama.cpp")).toBeInTheDocument();
  });

  it("shows persistent KV controls for an installed MLX chat model", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "list_targets") {
        return Promise.resolve([
          { id: "local-qwen", provider_id: null, name: "Qwen 3.5", kind: "mlx", wire_protocol: "open_ai_chat", provider_model: "qwen", local_path: "/models/qwen", runtime_url: "http://127.0.0.1:12100/v1", capabilities: ["chat", "streaming"], enabled: true, state: "ready", size_bytes: 4_000_000_000 },
        ]);
      }
      return previous?.(name, args);
    });
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Local models" }));
    fireEvent.click(await screen.findByRole("button", { name: "Library" }));
    expect(await screen.findByText("Qwen 3.5")).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("Resource overrides"));
    expect(await screen.findByRole("heading", { name: "Qwen 3.5 resources" })).toBeInTheDocument();
    expect(screen.getByText("Persistent KV (requires parallel = 1)")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Clear KV cache/ })).toBeInTheDocument();
    expect(screen.queryByText(/not supported by the MLX runtime/i)).not.toBeInTheDocument();
    expect(screen.getByText(/Metal/)).toBeInTheDocument();
  });

  it("enables adaptive routing per alias and keeps performance by default", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Custom routes" }));
    expect(await screen.findByRole("heading", { name: "Custom routes" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "adaptive-routing" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create route" }));
    expect(await screen.findByText("Create custom route")).toBeInTheDocument();
    fireEvent.focus(screen.getByRole("combobox", { name: "Primary" }));
    expect(screen.getByRole("option", { name: "Coding model OpenAI" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "adaptive-routing Built-in" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Primary" }), { target: { value: "adaptive" } });
    expect(screen.queryByRole("option", { name: "Coding model OpenAI" })).not.toBeInTheDocument();
    expect(screen.getByRole("option", { name: "adaptive-routing Built-in" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Performance routing for assistant" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "Adaptive routing for assistant" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByText("Performance", { selector: ".badge" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Adaptive routing for assistant" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({ policy: expect.objectContaining({ alias: "assistant", mode: "adaptive", status: "active", candidate_target_ids: ["cloud"] }) })));
    fireEvent.click(screen.getByRole("button", { name: "Configure" }));
    const heading = await screen.findByText("Routing · assistant");
    expect(heading.closest(".modal")).toHaveClass("wide");
    expect(screen.getByRole("button", { name: "Easy" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.queryByLabelText("quality weight")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Task hint")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Expert" }));
    expect(screen.getByLabelText("quality weight")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Adaptive routing$/ }));
    expect(screen.getByLabelText("Adaptive serving")).toHaveValue("active");
    fireEvent.change(screen.getByLabelText("Task hint"), { target: { value: "coding" } });
    fireEvent.change(screen.getByLabelText("Routing sample"), { target: { value: "write a Rust function" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByText(/adaptive · coding via header/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({ policy: expect.objectContaining({ alias: "assistant", mode: "adaptive", status: "active", candidate_target_ids: ["cloud"] }) })));
  });

  it("drops stale candidate ids when enabling adaptive routing", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "list_routing_policies") {
        return Promise.resolve([{
          version: 1, alias: "assistant", mode: "fixed", status: "draft", privacy: "local_preferred", default_task: "general",
          weights: { quality: .55, cost: .15, latency: .15, reliability: .10, locality: .05 },
          max_estimated_cost_usd: null, preferred_latency_ms: 2000, preferred_cost_usd: .01, rules: [],
          candidate_target_ids: ["cloud", "dae9cea9-c842-4a88-9d23-e0562d2d7646"],
        }]);
      }
      return previous?.(name, args);
    });
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Custom routes" }));
    fireEvent.click(await screen.findByRole("button", { name: "Adaptive routing for assistant" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({
      policy: expect.objectContaining({ alias: "assistant", mode: "adaptive", status: "active", candidate_target_ids: ["cloud"] }),
    })));
  });

  it("saves a primary pool with optional fallbacks", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "list_targets") {
        return Promise.resolve([
          { id: "cloud", provider_id: null, name: "Coding model", kind: "cloud", wire_protocol: "open_ai_chat", provider_model: "coding", local_path: null, runtime_url: null, capabilities: ["chat", "streaming"], enabled: true, state: "ready", size_bytes: null },
          { id: "vision", provider_id: null, name: "Vision model", kind: "cloud", wire_protocol: "open_ai_chat", provider_model: "gpt-4o", local_path: null, runtime_url: null, capabilities: ["chat", "streaming", "vision"], enabled: true, state: "ready", size_bytes: null },
        ]);
      }
      if (name === "list_routes") {
        return Promise.resolve([{
          alias: "assistant", enabled: true, capabilities: ["chat", "streaming", "vision"],
          targets: [
            { id: "cloud", kind: "cloud", model: "coding", priority: 10, enabled: true, role: "primary" },
            { id: "vision", kind: "cloud", model: "gpt-4o", priority: 20, enabled: true, role: "primary" },
            { id: "adaptive-routing", kind: "alias", model: "adaptive-routing", priority: 10, enabled: true, role: "fallback" },
          ],
        }]);
      }
      return previous?.(name, args);
    });
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Custom routes" }));
    expect(await screen.findByText("Primary 1")).toBeInTheDocument();
    expect(screen.getByText("Primary 2")).toBeInTheDocument();
    expect(screen.getByText("Fallback 1")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Configure" }));
    fireEvent.click(await screen.findByRole("button", { name: "Expert" }));
    expect(screen.getByText("Candidate primaries")).toBeInTheDocument();
    const candidates = screen.getByText("Candidate primaries").parentElement;
    expect(candidates).toHaveTextContent("Coding model");
    expect(candidates).toHaveTextContent("Vision model");
    expect(candidates).not.toHaveTextContent("adaptive-routing");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(screen.getByRole("button", { name: "Create route" }));
    expect(await screen.findByText("Create custom route")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("my-assistant"), { target: { value: "daily" } });
    fireEvent.click(screen.getByRole("button", { name: "Add primary" }));
    fireEvent.click(screen.getByRole("button", { name: "Add fallback" }));
    fireEvent.click(screen.getByRole("button", { name: "Save route" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_route", expect.objectContaining({
      route: expect.objectContaining({
        alias: "daily",
        targets: [
          expect.objectContaining({ id: "cloud", role: "primary", priority: 10 }),
          expect.objectContaining({ id: "vision", role: "primary", priority: 20 }),
          expect.objectContaining({ id: "adaptive-routing", role: "fallback", priority: 10 }),
        ],
      }),
    })));
  });

  it("saves performance routing from the alias card", async () => {
    const previous = command.getMockImplementation();
    command.mockImplementation((name: string, args?: unknown) => {
      if (name === "list_routing_policies") {
        return Promise.resolve([{
          version: 1, alias: "assistant", mode: "adaptive", status: "active", privacy: "local_preferred", default_task: "general",
          weights: { quality: .55, cost: .15, latency: .15, reliability: .10, locality: .05 },
          max_estimated_cost_usd: null, preferred_latency_ms: 2000, preferred_cost_usd: .01, rules: [],
          candidate_target_ids: ["cloud"],
        }]);
      }
      return previous?.(name, args);
    });
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Custom routes" }));
    expect(await screen.findByRole("button", { name: "Adaptive routing for assistant" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(screen.getByRole("button", { name: "Performance routing for assistant" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({
      policy: expect.objectContaining({ alias: "assistant", mode: "fixed", status: "draft" }),
    })));
  });

  it("accepts comma decimals in cost and price fields", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Custom routes" }));
    fireEvent.click(screen.getByRole("button", { name: "Configure" }));
    fireEvent.click(screen.getByRole("button", { name: "Expert" }));
    fireEvent.change(screen.getByLabelText("Preferred cost USD"), { target: { value: "0,0125" } });
    fireEvent.click(await screen.findByRole("button", { name: /Coding model/ }));
    fireEvent.change(screen.getByLabelText("Input USD / 1M"), { target: { value: "0,15" } });
    fireEvent.change(screen.getByLabelText("Output USD / 1M"), { target: { value: "0,60" } });
    fireEvent.click(screen.getByRole("button", { name: "Save profile" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_target_routing_profile", expect.objectContaining({
      profile: expect.objectContaining({ input_price_per_million: 0.15, output_price_per_million: 0.6 }),
    })));
    fireEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({
      policy: expect.objectContaining({ preferred_cost_usd: 0.0125 }),
    })));
  });
});
