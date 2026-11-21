#!/usr/bin/env node
// Verify that zh.json and en.json carry identical key sets (leaf paths).
// Exit 1 with the diff when they drift; otherwise print the key count.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const localesDir = path.join(root, "src", "locales");

function flatten(obj, prefix = "") {
  const keys = [];
  for (const [k, v] of Object.entries(obj)) {
    const p = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      keys.push(...flatten(v, p));
    } else {
      keys.push(p);
    }
  }
  return keys;
}

const read = (file) =>
  JSON.parse(readFileSync(path.join(localesDir, file), "utf8"));

const zh = flatten(read("zh.json"));
const en = flatten(read("en.json"));
const zhSet = new Set(zh);
const enSet = new Set(en);

const onlyZh = zh.filter((k) => !enSet.has(k));
const onlyEn = en.filter((k) => !zhSet.has(k));

if (onlyZh.length > 0 || onlyEn.length > 0) {
  console.error("check-locales: zh/en key sets differ");
  for (const k of onlyZh) console.error(`  only in zh.json: ${k}`);
  for (const k of onlyEn) console.error(`  only in en.json: ${k}`);
  process.exit(1);
}

console.log(`check-locales: OK (${zh.length} keys, zh/en identical)`);
