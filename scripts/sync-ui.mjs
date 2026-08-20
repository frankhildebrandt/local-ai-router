import { cpSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = join(root, "dist");
const dest = join(root, "src-tauri", "ui");

rmSync(dest, { recursive: true, force: true });
cpSync(src, dest, { recursive: true });
