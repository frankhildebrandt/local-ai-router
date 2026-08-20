import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

describe("browser admin transport", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
    vi.stubGlobal("EventSource", class {
      addEventListener() {}
      removeEventListener() {}
      close() {}
    });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("posts commands to the loopback admin API when Tauri is absent", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify({ running: true, base_url: "http://127.0.0.1:11435/v1" }),
    } as Response);
    const { command, isTauri } = await import("./api");
    expect(isTauri()).toBe(false);
    await expect(command("dashboard")).resolves.toEqual({
      running: true,
      base_url: "http://127.0.0.1:11435/v1",
    });
    expect(fetch).toHaveBeenCalledWith("/admin/dashboard", expect.objectContaining({ method: "POST" }));
  });
});
