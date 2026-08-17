import { invoke } from "@tauri-apps/api/core";

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export async function command<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isTauri()) throw new Error("Open this screen through the Local AI Router desktop app.");
  return invoke<T>(name, args);
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something went wrong";
}

