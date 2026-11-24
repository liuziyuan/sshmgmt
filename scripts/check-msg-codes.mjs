#!/usr/bin/env node
// Verify that the `msg.*` section of zh.json/en.json covers exactly the
// variants of `pub enum Msg` in src-tauri/src/model.rs (compared at one
// level: a variant key may be a string template or an object holding
// sub-templates such as `single`/`multi`, but no extra or missing variant
// keys are allowed either way).
//
// Bootstrapping tolerance: when model.rs has no `Msg` enum yet (S2 runs
// before S3) and the msg section is still empty, this script exits 0 with a
// hint. Once the enum exists, strict mode applies.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const modelRs = readFileSync(
  path.join(root, "src-tauri", "src", "model.rs"),
  "utf8",
);
const zh = JSON.parse(
  readFileSync(path.join(root, "src", "locales", "zh.json"), "utf8"),
);
const en = JSON.parse(
  readFileSync(path.join(root, "src", "locales", "en.json"), "utf8"),
);

// Extract variant names between `pub enum Msg {` and its closing brace.
// Lines are stripped of trailing // comments; attribute and doc lines are
// skipped. Matches `VariantName {`, `VariantName,` or `VariantName` alone.
function extractVariants(src) {
  const start = src.search(/^pub enum Msg \{/m);
  if (start === -1) return null;
  const body = src.slice(src.indexOf("{", start) + 1);
  let depth = 1;
  let end = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  const variants = [];
  for (const rawLine of body.slice(0, end).split("\n")) {
    let line = rawLine;
    const commentIdx = line.indexOf("//");
    if (commentIdx !== -1) line = line.slice(0, commentIdx);
    line = line.trim();
    if (!line || line.startsWith("#[")) continue;
    const m = line.match(/^([A-Z][A-Za-z0-9]*)\s*(\{|,|$)/);
    if (m) variants.push(m[1]);
  }
  return variants;
}

const toCamel = (s) => s.charAt(0).toLowerCase() + s.slice(1);

const variants = extractVariants(modelRs);
const msgKeys = Object.keys(zh.msg ?? {});

if (variants === null) {
  if (msgKeys.length === 0) {
    console.log(
      "check-msg-codes: no Msg enum in model.rs and empty msg section — " +
        "bootstrap OK (run again after S3 for strict mode)",
    );
    process.exit(0);
  }
  console.error(
    "check-msg-codes: locales have msg.* entries but model.rs has no Msg enum",
  );
  process.exit(1);
}

const expected = new Set(variants.map(toCamel));
const actual = new Set(msgKeys);

const missing = [...expected].filter((k) => !actual.has(k));
const extra = msgKeys.filter((k) => !expected.has(k));

const problems = [];
if (missing.length > 0)
  problems.push(
    `missing in locales msg.* (${missing.length}): ${missing.join(", ")}`,
  );
if (extra.length > 0)
  problems.push(`extra in locales msg.* (${extra.length}): ${extra.join(", ")}`);

// Every variant key must be translatable in both languages.
for (const [lang, data] of [["zh", zh], ["en", en]]) {
  for (const k of expected) {
    if (!(k in (data.msg ?? {}))) {
      problems.push(`${lang}.msg.${k} missing`);
    }
  }
}

if (problems.length > 0) {
  console.error(
    `check-msg-codes: Msg variants (${expected.size}) vs msg.* keys (${actual.size}) mismatch`,
  );
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}

console.log(
  `check-msg-codes: OK (${expected.size} Msg variants ↔ msg.* keys, zh/en both present)`,
);
