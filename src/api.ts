import { invoke } from "@tauri-apps/api/core";
import type { InstallJobEvent, InFlightRequest } from "./types";
import { version as packageVersion } from "../package.json";

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export async function appVersion(): Promise<string> {
  if (!isTauri()) return packageVersion;
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}

export async function command<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri()) return invoke<T>(name, args);
  const response = await fetch(`/admin/${name}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(args),
  });
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
  const text = await response.text();
  if (!text || text === "null") return undefined as T;
  return JSON.parse(text) as T;
}

function listenSse<T>(eventName: string, handler: (payload: T) => void): () => void {
  const source = new EventSource("/admin/events");
  const listener = (event: MessageEvent<string>) => {
    handler(JSON.parse(event.data) as T);
  };
  source.addEventListener(eventName, listener);
  return () => {
    source.removeEventListener(eventName, listener);
    source.close();
  };
}

export async function listenInstallJobs(handler: (event: InstallJobEvent) => void): Promise<() => void> {
  if (!isTauri()) return listenSse("install-job", handler);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<InstallJobEvent>("install-job", event => handler(event.payload));
}

export async function listenGatewayTraffic(handler: (inflight: InFlightRequest[]) => void): Promise<() => void> {
  if (!isTauri()) return listenSse("gateway-traffic", handler);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<InFlightRequest[]>("gateway-traffic", event => handler(event.payload));
}

export async function listenDesktopNavigate(handler: (page: string) => void): Promise<() => void> {
  if (!isTauri()) return () => undefined;
  const { listen } = await import("@tauri-apps/api/event");
  return listen<string>("desktop-navigate", event => handler(event.payload));
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something went wrong";
}

export function downloadTextFile(filename: string, contents: string, type = "text/plain") {
  const blob = new Blob([contents], { type });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}
