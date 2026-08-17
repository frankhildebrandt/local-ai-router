import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

const command = vi.fn();
vi.mock("./api", () => ({ command: (...args: unknown[]) => command(...args), errorMessage: (error: unknown) => String(error), isTauri: () => true }));

beforeEach(() => {
  command.mockImplementation((name: string) => {
    if (name === "dashboard") return Promise.resolve({ running: true, base_url: "http://127.0.0.1:11435/v1", provider_count: 0, target_count: 0, route_count: 0, recent_requests: 1, runtimes: [] });
    if (name === "list_providers" || name === "list_targets" || name === "list_routes") return Promise.resolve([]);
    if (name === "list_local_api_keys") return Promise.resolve([{ id: "default", name: "Default", created_at: "2026-08-17T10:00:00Z", last_used_at: null, revoked_at: null }]);
    if (name === "list_logs") return Promise.resolve({ total: 1, items: [{ id: "request", created_at: "2026-08-17T10:00:00Z", endpoint: "/v1/chat/completions", alias: "assistant", target: "cloud", attempts: 1, status: 200, latency_ms: 12, input_tokens: 3, output_tokens: 5, error_code: null, api_key_id: "default", api_key_name: "Default" }] });
    if (name === "get_settings") return Promise.resolve({});
    if (name === "get_log_facets") return Promise.resolve({ aliases: ["assistant"], targets: ["cloud"], endpoints: ["/v1/chat/completions"] });
    if (name === "get_usage") return Promise.resolve({ request_count: 1, success_count: 1, average_latency_ms: 12, input_tokens: 3, output_tokens: 5, unknown_usage_count: 0, buckets: [{ start: "2026-08-17T10:00:00Z", request_count: 1, input_tokens: 3, output_tokens: 5 }], by_key: [{ api_key_id: "default", api_key_name: "Default", request_count: 1, success_count: 1, average_latency_ms: 12, input_tokens: 3, output_tokens: 5, unknown_usage_count: 0 }] });
    if (name === "create_local_api_key") return Promise.resolve({ id: "new-key", name: "Automation", created_at: "2026-08-17T11:00:00Z", last_used_at: null, revoked_at: null, token: "lar_new" });
    return Promise.resolve(null);
  });
});
afterEach(cleanup);

describe("Local AI Router shell", () => {
  it("shows the private gateway quickstart without requiring a backend", async () => {
    render(<App />);
    expect(await screen.findByText("Your models, one local endpoint.")).toBeTruthy();
    expect(screen.getAllByText("http://127.0.0.1:11435/v1").length).toBeGreaterThan(0);
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
});
