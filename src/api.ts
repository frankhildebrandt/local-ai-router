import { invoke } from "@tauri-apps/api/core";
import type { InstallJobEvent } from "./types";

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export async function command<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isTauri()) throw new Error("Open this screen through the Local AI Router desktop app.");
  return invoke<T>(name, args);
}

export async function listenInstallJobs(handler: (event: InstallJobEvent) => void): Promise<() => void> {
  if (!isTauri()) return () => undefined;
  const { listen } = await import("@tauri-apps/api/event");
  return listen<InstallJobEvent>("install-job", event => handler(event.payload));
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something went wrong";
}
