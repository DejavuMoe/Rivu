#!/usr/bin/env node
// Design rule check for the dashboard and its prototype. No dependencies; run with `node tools/design-lint/check.mjs`.
//
// It enforces the mechanical part of apps/dashboard/DESIGN.md section 1, so a rule holds because a wrong value
// fails CI, not because someone remembered it:
//   raw-color    colors come from tokens.css only: no hex, rgb(), hsl(), and no Radix scale steps in components
//   font-size    only var(--fs-*)
//   font-weight  only var(--fw-*)
//   border       border widths are var(--hairline) or 0
//   radius       var(--r-*), or 0 / 1px / 2px / 50% / inherit
//   shadow       the shadow tokens, var(--focus-ring), or a hairline ring
//   effect       no gradients, no backdrop-filter, no blur in components (gradients live in tokens.css as tokens)
//   motion       transition and animation durations use var(--dur*)
//   middle-dot   no "·" in interface copy
//   drift        tokens and shared components are byte-identical in the prototype and in production
//                (suspended while an approved redesign waits to be implemented; see SYNC_SUSPENDED below)
//
// A deliberate exception stays next to the code, with its reason:
//   /* design-lint-allow raw-color: QR codes need a white background */     (CSS, same line or the line above)
//   <!-- design-lint-allow raw-color: ... -->                               (markup, the line above)
// One comment can name several rules: design-lint-allow raw-color effect: reason

import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const TREES = [
  { name: "dashboard", src: "apps/dashboard/src", lib: "apps/dashboard/src/lib", styles: "apps/dashboard/src/lib/styles" },
  { name: "prototype", src: "designs/rivu-svelte/src", lib: "designs/rivu-svelte/src/lib", styles: "designs/rivu-svelte/src/styles" },
];
// Token definitions: the only files allowed to contain raw values.
const TOKEN_FILES = new Set(["tokens.css", "radix.css"]);
// Shared between the two trees and required to stay identical.
const SHARED = [
  { dir: "lib", path: "ui" },
  { dir: "lib", path: "charts" },
  { dir: "styles", path: "tokens.css" },
  { dir: "styles", path: "radix.css" },
];
// The redesign approved on 2026-11-20 exists in the prototype only until apps/dashboard implements it, so the
// shared files differ on purpose. While this names the approved proposal, differences are listed but do not
// fail the check. Set it to null in the change that brings the dashboard to the new design.
const SYNC_SUSPENDED = "docs/ui/redesign-2026-11.md";
const RADIX_SCALES = "gray|mauve|slate|sage|olive|sand|tomato|red|ruby|crimson|pink|plum|purple|violet|iris|indigo|blue|cyan|teal|jade|green|grass|bronze|gold|brown|orange|amber|yellow|lime|mint|sky";

const problems = [];
const report = (file, line, rule, message) => problems.push({ file: relative(root, file).replaceAll("\\", "/"), line, rule, message });

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...walk(p));
    else out.push(p);
  }
  return out;
}

const lineOf = (text, index) => text.slice(0, index).split("\n").length;

/** Blank out CSS comments but keep line breaks, so indexes still map to lines. */
const stripCssComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));

/** Lines carrying `design-lint-allow <rule>`; the exception covers that line and the next one. */
function allowances(text) {
  const map = new Map();
  text.split("\n").forEach((line, i) => {
    for (const m of line.matchAll(/design-lint-allow ([a-z][a-z -]*)/g)) {
      for (const rule of m[1].trim().split(/ +/)) {
        for (const n of [i + 1, i + 2]) map.set(n, (map.get(n) ?? new Set()).add(rule));
      }
    }
  });
  return map;
}

/** Split CSS into declarations; selectors and at-rule preludes (anything ending at `{`) are skipped. */
function declarations(css) {
  const out = [];
  let buf = "";
  let start = 0;
  let depth = 0;
  for (let i = 0; i < css.length; i++) {
    const ch = css[i];
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (depth === 0 && (ch === "{" || ch === "}" || ch === ";")) {
      const colon = buf.indexOf(":");
      if (ch !== "{" && colon > 0) {
        const lead = buf.length - buf.trimStart().length;
        out.push({ prop: buf.slice(0, colon).trim().toLowerCase(), value: buf.slice(colon + 1).replace("!important", "").trim().replace(/\s+/g, " "), index: start + lead });
      }
      buf = "";
      start = i + 1;
    } else buf += ch;
  }
  return out;
}

function checkCss(file, fullText, css, offset) {
  const allow = allowances(fullText);
  const clean = stripCssComments(css);
  for (const d of declarations(clean)) {
    const line = lineOf(fullText, offset + d.index);
    const fail = (rule, message) => {
      if (!allow.get(line)?.has(rule)) report(file, line, rule, `${d.prop}: ${d.value} → ${message}`);
    };
    const { prop, value } = d;
    if (prop.startsWith("--")) continue;

    if (/#[0-9a-f]{3,8}\b/i.test(value) || /\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch|color-mix)\(/i.test(value)) {
      fail("raw-color", "use a semantic token from tokens.css");
    }
    if (new RegExp(`var\\(--(${RADIX_SCALES})-a?\\d+\\)`).test(value)) fail("raw-color", "use a semantic token, not a Radix scale step");

    if (prop === "font-size" && !/^(var\(--fs-(sm|md|lg|xl|2xl|3xl)\)|inherit)$/.test(value)) fail("font-size", "use var(--fs-sm|md|lg|xl|2xl|3xl)");
    if (prop === "font-weight" && !/^(var\(--fw-(regular|medium|semibold)\)|inherit)$/.test(value)) fail("font-weight", "use var(--fw-regular|medium|semibold)");
    if (prop === "font" && !/^inherit$/.test(value)) fail("font-size", "set font-size and font-weight with tokens instead of the font shorthand");

    if (/^border(-(top|right|bottom|left|block|inline)(-(start|end))?)?(-width)?$/.test(prop) && /(?<![\w.-])\d*\.?\d+px/.test(value)) {
      fail("border", "use var(--hairline) (0.5px) or 0");
    }
    if (/^border(-[a-z-]+)?-radius$/.test(prop)) {
      const bad = value.split(/[\s/]+/).filter((v) => !/^(var\(--r-(xs|sm|md|lg|full)\)|0|1px|2px|50%|inherit)$/.test(v));
      if (bad.length) fail("radius", "use var(--r-xs|sm|md|lg|full)");
    }
    if (prop === "box-shadow") {
      const ok = value.split(/,(?![^(]*\))/).every((part) => /^(none|var\(--(shadow-pop|shadow-card|shadow-control|focus-ring)\)|(inset )?0 0 0 (var\(--hairline\)|1px|2px) var\(--[a-z-]+\))$/.test(part.trim()));
      if (!ok) fail("shadow", "only the shadow tokens (--shadow-pop, --shadow-card, --shadow-control), var(--focus-ring), or a hairline ring");
    }
    if (/gradient\(/.test(value) || prop === "backdrop-filter" || (prop === "filter" && /blur\(/.test(value))) {
      fail("effect", "no gradients, glass or blur");
    }
    if (/^(transition|animation)(-duration)?$/.test(prop) && /(?<![\w.-])\d*\.?\d+m?s\b/.test(value.replace(/var\([^)]*\)/g, ""))) {
      fail("motion", "use a duration token (--dur-fast, --dur-pop, --dur, --dur-arrive, --dur-draw, --dur-pulse)");
    }
  }
}

function checkMarkup(file, text) {
  const allow = allowances(text);
  // Blank out scripts, styles and comments; what is left is markup and visible text.
  const blank = (m) => m.replace(/[^\n]/g, " ");
  const markup = text.replace(/<script[\s\S]*?<\/script>/g, blank).replace(/<style[\s\S]*?<\/style>/g, blank).replace(/<!--[\s\S]*?-->/g, blank);
  for (const m of markup.matchAll(/\b(fill|stroke|color|stop-color|style)=["'{][^"'}]*(#[0-9a-f]{3,8}\b|rgba?\()/gi)) {
    const line = lineOf(text, m.index);
    if (!allow.get(line)?.has("raw-color")) report(file, line, "raw-color", `${m[0]} → use a token (currentColor or var(--…))`);
  }
  for (const m of markup.matchAll(/·/g)) {
    const line = lineOf(text, m.index);
    if (!allow.get(line)?.has("middle-dot")) report(file, line, "middle-dot", "no “·” as a separator in interface copy");
  }
}

function checkStrings(file, text) {
  const allow = allowances(text);
  for (const m of text.matchAll(/(["'`])(?:(?!\1)[^\\\n]|\\.)*·(?:(?!\1)[^\\\n]|\\.)*\1/g)) {
    const line = lineOf(text, m.index);
    if (!allow.get(line)?.has("middle-dot")) report(file, line, "middle-dot", "no “·” as a separator in interface copy");
  }
}

for (const tree of TREES) {
  for (const file of walk(join(root, tree.src))) {
    const name = file.split(/[\\/]/).pop();
    if (TOKEN_FILES.has(name)) continue;
    const text = readFileSync(file, "utf8");
    if (name.endsWith(".css")) checkCss(file, text, text, 0);
    else if (name.endsWith(".svelte")) {
      for (const m of text.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)) checkCss(file, text, m[1], m.index + m[0].indexOf(m[1]));
      checkMarkup(file, text);
    } else if (/(^i18n|strings)\.ts$/.test(name)) checkStrings(file, text);
  }
}

// Prototype and production share tokens and components by copy; a silent edit on one side is drift.
const [a, b] = TREES;
for (const item of SHARED) {
  const pa = join(root, a[item.dir], item.path);
  const pb = join(root, b[item.dir], item.path);
  const list = (p) => (statSync(p).isDirectory() ? walk(p).map((f) => relative(p, f)) : [""]);
  const names = new Set([...list(pa), ...list(pb)]);
  for (const name of names) {
    const fa = join(pa, name);
    const fb = join(pb, name);
    let ta;
    let tb;
    try {
      ta = readFileSync(fa, "utf8").replaceAll("\r\n", "\n");
    } catch {
      report(fb, 1, "drift", `exists in the ${b.name} only; copy it to ${relative(root, fa).replaceAll("\\", "/")}`);
      continue;
    }
    try {
      tb = readFileSync(fb, "utf8").replaceAll("\r\n", "\n");
    } catch {
      report(fa, 1, "drift", `exists in the ${a.name} only; copy it to ${relative(root, fb).replaceAll("\\", "/")}`);
      continue;
    }
    if (ta !== tb) {
      const la = ta.split("\n");
      const lb = tb.split("\n");
      const at = la.findIndex((l, i) => l !== lb[i]);
      report(fa, (at < 0 ? Math.min(la.length, lb.length) : at) + 1, "drift", `differs from ${relative(root, fb).replaceAll("\\", "/")}`);
    }
  }
}

const drift = SYNC_SUSPENDED ? problems.filter((p) => p.rule === "drift") : [];
if (drift.length) {
  for (const p of drift) problems.splice(problems.indexOf(p), 1);
  console.log(`Sync check suspended (${SYNC_SUSPENDED}): ${drift.length} shared file(s) differ between the prototype and the dashboard.`);
}

if (problems.length) {
  for (const p of problems) console.error(`${p.file}:${p.line}  ${p.rule}  ${p.message}`);
  console.error(`\n${problems.length} design rule problem(s). Rules: apps/dashboard/DESIGN.md section 1.`);
  process.exit(1);
}
console.log(`Design rules pass (registered exceptions aside): tokens only, token font sizes, hairline borders, no gradients in components${SYNC_SUSPENDED ? "" : ", prototype and production in sync"}.`);
