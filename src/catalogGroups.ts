import type { CatalogEntry, RamFit } from "./types";

const QUANT_SUFFIX = /(?:[-_ ](?:mlx[-_ ]?)?)?(?:q[2-8](?:_[kmxs]+)?(?:_[msxl])?|\d+-?bit|mxfp4(?:-q[2-8])?|bf16|fp16|fp32|int[48]|nf4)$/i;

export function displayModelName(entry: CatalogEntry): string {
  const stripped = entry.name.replace(QUANT_SUFFIX, "").replace(/[-_ ]+$/, "").trim();
  return stripped || entry.name;
}

export function quantizationGroupKey(entry: CatalogEntry): string {
  return [entry.category, entry.family, displayModelName(entry).toLowerCase(), [...entry.capabilities].sort().join(",")].join("|");
}

export function preferredQuantization(entries: CatalogEntry[]): CatalogEntry {
  return [...entries].sort(compareQuantizations)[0] ?? entries[0];
}

export function groupCatalogEntries(entries: CatalogEntry[]): CatalogEntry[][] {
  const groups = new Map<string, CatalogEntry[]>();
  for (const entry of entries) {
    const key = quantizationGroupKey(entry);
    const group = groups.get(key);
    if (group) group.push(entry);
    else groups.set(key, [entry]);
  }
  return [...groups.values()].map(group => [...group].sort(compareQuantizations));
}

function compareQuantizations(left: CatalogEntry, right: CatalogEntry): number {
  const ram = ramRank(left.ram_fit) - ramRank(right.ram_fit);
  if (ram) return ram;
  if (left.ram_fit === "fits") return quantizationQuality(right) - quantizationQuality(left);
  return left.estimated_memory_bytes - right.estimated_memory_bytes;
}

function ramRank(fit: RamFit): number {
  if (fit === "fits") return 0;
  if (fit === "tight") return 1;
  if (fit === "unsuitable") return 2;
  return 3;
}

function quantizationQuality(entry: CatalogEntry): number {
  const text = `${entry.quantization} ${entry.repo_id}`.toLowerCase();
  if (text.includes("fp32")) return 100;
  if (text.includes("bf16") || text.includes("fp16")) return 80;
  if (text.includes("8-bit") || text.includes("8bit") || /\bq8\b/.test(text)) return 50;
  if (text.includes("6-bit") || text.includes("6bit") || /\bq6\b/.test(text)) return 40;
  if (text.includes("5-bit") || text.includes("5bit") || /\bq5\b/.test(text)) return 35;
  if (text.includes("mxfp4") || text.includes("4-bit") || text.includes("4bit") || /\bq4\b/.test(text)) return 30;
  if (text.includes("3-bit") || text.includes("3bit") || /\bq3\b/.test(text)) return 20;
  if (text.includes("2-bit") || text.includes("2bit") || /\bq2\b/.test(text)) return 10;
  return 25;
}
