//! What the browser suite actually executes in `static/*.js`.
//!
//! `cargo llvm-cov` measures `src/`. It cannot see a line of the front end —
//! and the front end is a fifth of the shipped code — so quoting the Rust
//! figure as "the project's coverage" overstates it by exactly the part no
//! Rust test can reach.
//!
//! This runs every browser test with `ROOST_JS_COV` set, which makes each
//! page dump its V8 precise-coverage report as it closes, then unions the
//! covered byte ranges across all of them.
//!
//! **The union is the point.** Each test drives a narrow slice and they all
//! share the same startup path, so averaging per-test percentages would count
//! that path forty times and report a number far above the truth. Coverage is
//! a property of the file, measured once, over the union of what every test
//! reached.
//!
//! Byte ranges, not lines: V8 reports offsets, and converting to lines would
//! mean calling a line covered because one expression on it ran. Bytes are
//! what was measured, so bytes are what is reported.
//!
//! # Instrumentation is not free, and it changes what some tests measure
//!
//! V8's precise coverage slows the instrumented page down, and a few tests
//! here are throughput-bound rather than event-bound. `altscreen.mjs` pushes
//! 1 MB of terminal output to turn the scrollback ring over: it passes in 8
//! seconds normally and times out after 318 under coverage, because the
//! output never arrives in time. Its later sections then cascade.
//!
//! So a failure in this run is not automatically a regression — check the
//! same file without `ROOST_JS_COV` before believing it — and the total is a
//! slight **under**-estimate, since a test that dies early contributes only
//! what it reached. Neither is worth fixing by loosening the deadlines: they
//! are what those tests are for.
//!
//! Run: deno run -A tests/browser/coverage.mjs [--only name,name]

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
const covFile = await Deno.makeTempFile({ prefix: "roost-jscov-" });

const only = (() => {
  const i = Deno.args.indexOf("--only");
  return i >= 0 && Deno.args[i + 1] ? new Set(Deno.args[i + 1].split(",")) : null;
})();

const tests = [];
for await (const e of Deno.readDir(`${repoRoot}/tests/browser`)) {
  if (!e.isFile || !e.name.endsWith(".mjs")) continue;
  if (["harness.mjs", "coverage.mjs"].includes(e.name)) continue;
  const stem = e.name.replace(/\.mjs$/, "");
  if (only && !only.has(stem)) continue;
  tests.push(e.name);
}
tests.sort();

console.log(`running ${tests.length} browser tests with coverage on\n`);
const failed = [];
for (const t of tests) {
  const started = Date.now();
  const p = new Deno.Command("deno", {
    args: ["run", "-A", `${repoRoot}/tests/browser/${t}`],
    env: { ...Deno.env.toObject(), ROOST_JS_COV: covFile },
    stdout: "piped", stderr: "piped",
  });
  const { code } = await p.output();
  const secs = ((Date.now() - started) / 1000).toFixed(0);
  if (code !== 0) failed.push(t);
  console.log(`  ${code === 0 ? "ok  " : "FAIL"}  ${t.padEnd(26)} ${secs}s`);
}

// --- union ----------------------------------------------------------------
//
// V8 reports each function's ranges, and a nested range with count 0 carves a
// hole out of the enclosing one that ran. So "covered" cannot be taken from
// the outermost range alone: an `if` body that never executed sits inside a
// function that did. Ranges are applied in order, later (more deeply nested)
// ones overwriting earlier, which is V8's own precedence.
const files = new Map(); // url -> { covered: Uint8Array, length }
for (const line of (await Deno.readTextFile(covFile)).split("\n")) {
  if (!line.trim()) continue;
  let entry;
  try { entry = JSON.parse(line); } catch { continue; }
  // Keyed by the path under /static/, never the full URL: every test starts
  // roost on its own free port, so keying by URL gives one entry per test and
  // the union — the entire point of this script — silently does not happen.
  // The first working run reported app.js three times, at 44%, 48% and 50%.
  const bare = entry.url.replace(/\?.*$/, "");
  const url = bare.includes("/static/") ? bare.slice(bare.indexOf("/static/")) : bare;
  let end = 0;
  for (const f of entry.functions ?? []) {
    for (const r of f.ranges ?? []) end = Math.max(end, r.endOffset);
  }
  if (!files.has(url)) files.set(url, { covered: new Uint8Array(end), length: end });
  const f = files.get(url);
  if (end > f.length) {
    const bigger = new Uint8Array(end);
    bigger.set(f.covered);
    files.set(url, { covered: bigger, length: end });
  }
  const cur = files.get(url);
  for (const fn of entry.functions ?? []) {
    for (const r of fn.ranges ?? []) {
      const v = r.count > 0 ? 1 : 0;
      for (let i = r.startOffset; i < r.endOffset && i < cur.length; i++) cur.covered[i] = v;
    }
  }
}

console.log("\n--- static/*.js, union of every test ---\n");
let totalBytes = 0, totalCovered = 0;
const rows = [];
for (const [url, f] of [...files].sort()) {
  const name = url.replace("/static/", "");
  let covered = 0;
  for (const b of f.covered) covered += b;
  totalBytes += f.length;
  totalCovered += covered;
  rows.push({ name, length: f.length, covered, pct: (100 * covered) / f.length });
}
for (const r of rows.sort((a, b) => a.pct - b.pct)) {
  console.log(`  ${r.name.padEnd(16)} ${String(r.length).padStart(8)} bytes  ${r.covered.toString().padStart(8)} covered  ${r.pct.toFixed(2).padStart(6)}%`);
}
const pct = totalBytes ? (100 * totalCovered) / totalBytes : 0;
console.log(`\n  ${"TOTAL".padEnd(16)} ${String(totalBytes).padStart(8)} bytes  ${totalCovered.toString().padStart(8)} covered  ${pct.toFixed(2).padStart(6)}%`);
if (failed.length) console.log(`\n  ${failed.length} test file(s) failed: ${failed.join(", ")}`);
console.log(`\n  raw: ${covFile}`);
