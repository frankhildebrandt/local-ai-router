import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

const command = vi.fn();
vi.mock("./api", () => ({ command: (...args: unknown[]) => command(...args), listenInstallJobs: async () => () => undefined, errorMessage: (error: unknown) => String(error), isTauri: () => true }));

beforeEach(() => {
  command.mockImplementation((name: string) => {
    if (name === "dashboard") return Promise.resolve({ running: true, base_url: "http://127.0.0.1:11435/v1", provider_count: 0, target_count: 0, route_count: 0, recent_requests: 1, runtimes: [] });
    if (name === "list_providers") return Promise.resolve([]);
    if (name === "list_targets") return Promise.resolve([{ id: "cloud", provider_id: null, name: "Coding model", kind: "cloud", wire_protocol: "open_ai_chat", provider_model: "coding", local_path: null, runtime_url: null, capabilities: ["chat", "streaming"], enabled: true, state: "ready", size_bytes: null }]);
    if (name === "list_routes") return Promise.resolve([{ alias: "assistant", enabled: true, capabilities: ["chat", "streaming"], targets: [{ id: "cloud", kind: "cloud", model: "coding", priority: 10, enabled: true }] }]);
    if (name === "list_routing_policies" || name === "list_target_routing_profiles") return Promise.resolve([]);
    if (name === "list_routing_attempts") return Promise.resolve([]);
    if (name === "list_routing_tasks") return Promise.resolve([{ id: "general", label: "general", builtin: true }, { id: "coding", label: "coding", builtin: true }]);
    if (name === "simulate_routing") return Promise.resolve({ alias: "assistant", mode: "fixed", task: "coding", task_source: "header", ordered_target_ids: ["cloud"], shadow_target_id: null, estimated_input_tokens: 3, decision: { task: "coding", ranked: [{ target_id: "cloud", score: { quality: .9, cost: .5, latency: .5, reliability: .95, locality: 0, total: .8 }, estimated_cost_usd: null, cost_verified: false }], excluded: [] } });
    if (name === "export_routing_config") return Promise.resolve({ schema: "local-ai-router/routing-policy/v1", tasks: [], profiles: [], policies: [] });
    if (name === "list_provider_presets") return Promise.resolve([]);
    if (name === "list_local_api_keys") return Promise.resolve([{ id: "default", name: "Default", created_at: "2026-08-17T10:00:00Z", last_used_at: null, revoked_at: null }]);
    if (name === "list_logs") return Promise.resolve({ total: 1, items: [{ id: "request", created_at: "2026-08-17T10:00:00Z", endpoint: "/v1/chat/completions", alias: "assistant", target: "cloud", attempts: 1, status: 200, latency_ms: 12, input_tokens: 3, output_tokens: 5, error_code: null, api_key_id: "default", api_key_name: "Default" }] });
    if (name === "get_settings") return Promise.resolve({});
    if (name === "get_resource_policy") return Promise.resolve({ version: 1, profile: "stealth", memory_budget_percent: 50, memory_budget_mib: null, auto_load: true, idle_unload_minutes: 5, compute_duty_percent: 25, cpu_threads: 4, max_parallel_prompts: 1, process_priority: -1, gguf_gpu_layers: -1, disk_kv_enabled: true, disk_kv_max_bytes: 10 * 1024 ** 3 });
    if (name === "get_resource_profile_preset") return Promise.resolve({ version: 1, profile: "balanced", memory_budget_percent: 70, memory_budget_mib: null, auto_load: true, idle_unload_minutes: 15, compute_duty_percent: 60, cpu_threads: 8, max_parallel_prompts: 2, process_priority: 0, gguf_gpu_layers: -1, disk_kv_enabled: false, disk_kv_max_bytes: 10 * 1024 ** 3 });
    if (name === "get_log_facets") return Promise.resolve({ aliases: ["assistant"], targets: ["cloud"], endpoints: ["/v1/chat/completions"] });
    if (name === "get_usage") return Promise.resolve({ request_count: 1, success_count: 1, average_latency_ms: 12, input_tokens: 3, output_tokens: 5, unknown_usage_count: 0, buckets: [{ start: "2026-08-17T10:00:00Z", request_count: 1, input_tokens: 3, output_tokens: 5 }], by_key: [{ api_key_id: "default", api_key_name: "Default", request_count: 1, success_count: 1, average_latency_ms: 12, input_tokens: 3, output_tokens: 5, unknown_usage_count: 0 }] });
    if (name === "create_local_api_key") return Promise.resolve({ id: "new-key", name: "Automation", created_at: "2026-08-17T11:00:00Z", last_used_at: null, revoked_at: null, token: "lar_new" });
    if (name === "client_chat") return Promise.resolve({ model: "assistant", content: "Hello from the router" });
    if (name === "list_local_catalog") return Promise.resolve({
      platform: { apple_silicon: true, macos_15_plus: true, compatible: true, reason: null },
      memory_budget_bytes: 24 * 1024 * 1024 * 1024,
      memory_budget_percent: 70,
      entries: [
        { id: "qwen-3-5-4b", name: "Qwen 3.5 4B", family: "Qwen 3.5", repo_id: "mlx-community/Qwen3.5-4B-MLX-4bit", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "4-bit", license: "apache-2.0", alias: "qwen-3-5-4b", capabilities: ["chat", "streaming"], download_bytes: 3_000_000_000, estimated_memory_bytes: 4_000_000_000, ram_fit: "fits", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "gemma-4-audio", name: "Gemma 4 Audio", family: "Gemma 4", repo_id: "mlx-community/gemma-4-audio", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "4-bit", license: "gemma", alias: "gemma-4-audio", capabilities: ["chat", "audio_input"], download_bytes: 4_000_000_000, estimated_memory_bytes: 6_000_000_000, ram_fit: "tight", trust_status: "curated", installable: false, lock_reason: "Audio input is locked until upstream support is stable", voices: [], gated: false },
        { id: "flux2-klein", name: "FLUX.2 Klein 4B", family: "FLUX.2", repo_id: "mlx-community/FLUX.2-klein-4B-4bit", category: "image", task: "image", runtime_engine: "mlx_image", quantization: "4-bit", license: "flux-1-dev", alias: "flux2-klein", capabilities: ["images"], download_bytes: 8_000_000_000, estimated_memory_bytes: 16_000_000_000, ram_fit: "unsuitable", trust_status: "curated", installable: true, lock_reason: null, voices: [], gated: false },
        { id: "kokoro", name: "Kokoro 82M", family: "Kokoro", repo_id: "mweinbach/Kokoro-82M-Swift", category: "speech", task: "speech", runtime_engine: "mlx_speech", quantization: "fp16", license: "apache-2.0", alias: "kokoro", capabilities: ["speech"], download_bytes: 350_000_000, estimated_memory_bytes: 500_000_000, ram_fit: "fits", trust_status: "curated", installable: true, lock_reason: null, voices: ["af_heart", "af_bella"], gated: false },
      ],
    });
    if (name === "list_install_jobs") return Promise.resolve([{ id: "job-1", repo_id: "mlx-community/Qwen3.5-2B-4bit", revision: "abc", status: "downloading", catalog_id: "qwen-3-5-2b", alias: "qwen-3-5-2b", engine: "mlx_chat", task: "chat", capabilities: ["chat"], bytes_downloaded: 500, bytes_total: 1000, current_file: "model.safetensors", staging_dir: "/tmp", error: null, confirm_over_budget: false, created_at: "2026-08-17T10:00:00Z", updated_at: "2026-08-17T10:00:00Z" }]);
    if (name === "search_mlx_catalog") return Promise.resolve({ items: [{ id: "org/untested", name: "Untested MLX", family: "Unknown", repo_id: "org/untested", category: "chat_vision", task: "chat", runtime_engine: "mlx_chat", quantization: "unknown", license: "unknown", alias: "untested", capabilities: ["chat"], download_bytes: 1, estimated_memory_bytes: 2, ram_fit: "fits", trust_status: "untested", installable: false, lock_reason: "unknown architecture", voices: [], gated: false }], next_cursor: null });
    if (name === "inspect_mlx_model") return Promise.resolve({ repo_id: "org/untested", revision: "main", model_type: "mystery", pipeline_tag: "text-generation", license: "mit", gated: false, mlx_format: true, download_bytes: 1, files: ["config.json"], runtime_engine: null, task: null, category: "chat_vision", capabilities: [], estimated_memory_bytes: 2, ram_fit: "fits", installable: false, blockers: ["unknown architecture"], trust_status: "untested" });
    if (name === "install_catalog_model") return Promise.resolve({ id: "job-2", repo_id: "mlx-community/Qwen3.5-4B-MLX-4bit", revision: "abc", status: "queued", catalog_id: "qwen-3-5-4b", alias: null, engine: "mlx_chat", task: "chat", capabilities: ["chat"], bytes_downloaded: 0, bytes_total: 1, current_file: null, staging_dir: null, error: null, confirm_over_budget: false, created_at: "2026-08-17T10:00:00Z", updated_at: "2026-08-17T10:00:00Z" });
    return Promise.resolve(null);
  });
});
afterEach(cleanup);

describe("Local AI Router shell", () => {
  it("opens the preconfigured chat and sends through the selected alias", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    expect(await screen.findByText("Start a private conversation")).toBeInTheDocument();
    expect(screen.getByLabelText("Model alias")).toHaveValue("assistant");
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("client_chat", expect.objectContaining({ input: expect.objectContaining({ model: "assistant" }) })));
    expect(await screen.findByText("Hello from the router")).toBeInTheDocument();
  });

  it("shows the private gateway quickstart without requiring a backend", async () => {
    render(<App />);
    expect(await screen.findByText("Your models, one local endpoint.")).toBeTruthy();
    expect(screen.getAllByText("http://127.0.0.1:11435/v1").length).toBeGreaterThan(0);
    expect(command).toHaveBeenCalledWith("list_logs", { query: { legacy_only: false, limit: 100 } });
  });

  it("exposes usage, named keys, and structured request-log filters", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");

    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    expect(await screen.findByText("Usage by API key")).toBeInTheDocument();
    expect(screen.getByText("Input tokens")).toBeInTheDocument();
    expect(screen.getByText("Output tokens")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "24h" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("get_usage", { period: "24h" }));

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByText("Local API keys")).toBeInTheDocument();
    expect(screen.getByLabelText("Inference profile")).toHaveValue("stealth");
    fireEvent.change(screen.getByLabelText("Inference profile"), { target: { value: "balanced" } });
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_resource_policy", expect.objectContaining({ policy: expect.objectContaining({ profile: "balanced", compute_duty_percent: 60, max_parallel_prompts: 2 }) })));
    expect(screen.getByText("Default")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("New key name"), { target: { value: "Automation" } });
    fireEvent.click(screen.getByRole("button", { name: "Create key" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("create_local_api_key", { name: "Automation" }));

    fireEvent.click(screen.getByRole("button", { name: "Request logs" }));
    expect(await screen.findByLabelText("API key")).toBeInTheDocument();
    expect(screen.getByLabelText("Status")).toBeInTheDocument();
    expect(await screen.findByText("1 matching request")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Status"), { target: { value: "5xx" } });
    await waitFor(() => expect(command).toHaveBeenCalledWith("list_logs", expect.objectContaining({ query: expect.objectContaining({ status_class: "5xx" }) })));
  });

  it("shows curated catalog categories, RAM badges, locked models, and import paths", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Local models" }));
    expect(await screen.findByText("Qwen 3.5 4B")).toBeInTheDocument();
    expect(screen.getByText("Fits")).toBeInTheDocument();
    expect(screen.getAllByText("curated").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Chat & Vision" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("install_catalog_model", expect.objectContaining({ input: expect.objectContaining({ repoId: "mlx-community/Qwen3.5-4B-MLX-4bit" }) })));
    expect(screen.getByText("Gemma 4 Audio")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Locked" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Image" }));
    expect(await screen.findByText("FLUX.2 Klein 4B")).toBeInTheDocument();
    expect(screen.getByText("Unsuitable")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Speech" }));
    expect(await screen.findByText("Kokoro 82M")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("Search curated models or every Hugging Face MLX repository"), { target: { value: "mystery" } });
    expect(await screen.findByText("Untested Hugging Face matches")).toBeInTheDocument();
    expect(screen.getByText("Untested MLX")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    expect(screen.getByRole("button", { name: "Hugging Face" })).toBeInTheDocument();
    expect(screen.getByText("GGUF · llama.cpp")).toBeInTheDocument();
  });

  it("configures and previews an adaptive routing policy before activation", async () => {
    render(<App />);
    await screen.findByText("Your models, one local endpoint.");
    fireEvent.click(screen.getByRole("button", { name: "Aliases & routes" }));
    expect(await screen.findByText("Aliases & adaptive policies")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Policy" }));
    expect(await screen.findByText("Adaptive policy · assistant")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Routing mode"), { target: { value: "adaptive" } });
    expect(screen.getByLabelText("Policy activation")).toHaveValue("draft");
    fireEvent.change(screen.getByLabelText("Task hint"), { target: { value: "coding" } });
    fireEvent.change(screen.getByLabelText("Routing sample"), { target: { value: "write a Rust function" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByText(/fixed · coding via header/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Policy activation"), { target: { value: "shadow" } });
    fireEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith("save_routing_policy", expect.objectContaining({ policy: expect.objectContaining({ alias: "assistant", mode: "adaptive", status: "shadow" }) })));
  });
});
