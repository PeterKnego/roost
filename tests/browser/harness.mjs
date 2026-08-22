//! Browser-test harness: a real Chromium, driven over CDP, against a real
//! resh with real dtach.
//!
//! It exists because this project's worst defects have been invisible to a
//! green `cargo test` — CLAUDE.md lists four of them, each hidden by a
//! substitution the suite makes for the real thing (`RESH_CMD=cat` for dtach,
//! no browser at all). The terminal reconnect is the same shape: every line of
//! it lives in static/app.js, where `cargo test` cannot reach.
//!
//! Everything here is deliberately hermetic. The harness builds resh, starts
//! its *own* instance on a free port with its own state directory and a
//! throwaway project, and tears the lot down afterwards. It never touches a
//! deployed or development instance, so a test run cannot kill a session
//! someone is using.
const enc = new TextEncoder();

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// Polls until `fn()` is truthy. Returns false on timeout rather than
/// throwing, so a caller can assert on it and keep going: knowing which of
/// six steps failed is worth more than the first exception.
export async function until(fn, secs, label) {
  for (let i = 0; i < secs * 10; i++) {
    try { if (await fn()) return true; } catch { /* not ready yet */ }
    await sleep(100);
  }
  if (label) console.log(`    (timed out waiting for ${label})`);
  return false;
}

export async function freePort() {
  const l = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const { port } = l.addr;
  l.close();
  return port;
}

/// Kills processes whose command line contains `needle`.
///
/// Deliberately not `pkill -f`: that matches the *invoking shell's own*
/// command line, which contains the pattern as an argument, so it kills the
/// caller. (Observed three times while developing this harness.) Reading
/// /proc directly and skipping our own pid has no such hazard.
export async function killByCmdline(needle) {
  let killed = 0;
  for await (const e of Deno.readDir("/proc")) {
    const pid = Number(e.name);
    if (!Number.isInteger(pid) || pid === Deno.pid) continue;
    let cmd;
    try { cmd = await Deno.readTextFile(`/proc/${pid}/cmdline`); } catch { continue; }
    if (!cmd.replaceAll("\0", " ").includes(needle)) continue;
    try { Deno.kill(pid, "SIGTERM"); killed++; } catch { /* already gone */ }
  }
  return killed;
}

// ---------------------------------------------------------------- browser

/// Chromium is found, never installed. A missing browser skips the run with
/// an actionable message instead of failing: this suite is opt-in, and a
/// machine without a browser is a normal state, not a broken one.
export async function findBrowser() {
  if (Deno.env.get("CHROME")) return Deno.env.get("CHROME");
  for (const c of ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"]) {
    try {
      const p = new Deno.Command("sh", { args: ["-c", `command -v ${c}`], stdout: "piped" });
      const { code, stdout } = await p.output();
      if (code === 0) return new TextDecoder().decode(stdout).trim();
    } catch { /* keep looking */ }
  }
  return null;
}

export async function startBrowser(profileDir) {
  const bin = await findBrowser();
  if (!bin) {
    console.log("SKIP: no chromium found. `snap install chromium`, or set CHROME=/path/to/chrome.");
    Deno.exit(0);
  }
  const port = await freePort();
  await Deno.mkdir(profileDir, { recursive: true });
  const proc = new Deno.Command(bin, {
    args: [
      "--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
      `--user-data-dir=${profileDir}`, `--remote-debugging-port=${port}`,
    ],
    stdout: "null", stderr: "null",
  }).spawn();
  const up = await until(async () => {
    try { return (await fetch(`http://127.0.0.1:${port}/json/version`)).ok; } catch { return false; }
  }, 30, "chromium's debug port");
  if (!up) { try { proc.kill("SIGKILL"); } catch {} throw new Error(`${bin} never opened its debug port`); }
  return { bin, port, close: () => { try { proc.kill("SIGKILL"); } catch {} } };
}

/// Opens a tab and returns a thin CDP client for it. `evalIn` runs an
/// expression in the page and returns its value; that is the whole interface
/// the tests need, because everything they assert about lives in app.js's own
/// state (`terms`, an entry's socket, the xterm buffer).
export async function openPage(cdpPort, url) {
  const t = await (await fetch(`http://127.0.0.1:${cdpPort}/json/new?${url}`, { method: "PUT" })).json();
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  let id = 0;
  const pending = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    const p = pending.get(m.id);
    if (!p) return;
    clearTimeout(p.timer);
    pending.delete(m.id);
    p.res(m);
  };
  // A CDP command that never gets a reply used to wedge the run forever:
  // resolved from `pending` with no timeout and no reject path, so the suite
  // stopped dead mid-file with no failing assertion and no output. That is
  // CLAUDE.md's "a deadlock hangs rather than fails" applied to the harness
  // itself — CI shows a run that never ends, not a test that went red.
  //
  // Not hypothetical: termlinks.mjs hit it. xterm's fallback for an OSC 8
  // link with no linkHandler calls confirm(), and a native dialog with no
  // Page.javascriptDialogOpening handler wedges the renderer, so
  // Input.dispatchMouseEvent never replied. A crashed renderer or a dropped
  // frame does the same to any file here.
  //
  // 30s is far past any legitimate command — the slowest is the initial page
  // load — so a timeout means something is wrong, never that a command was
  // merely slow. The method name is in the message because the whole point
  // is to turn a wall-clock mystery into a legible failure.
  const CMD_TIMEOUT_MS = 30_000;
  const cmd = (method, params = {}) =>
    new Promise((res, rej) => {
      const i = ++id;
      const timer = setTimeout(() => {
        pending.delete(i);
        rej(new Error(`CDP command ${method} (id ${i}) got no reply in ${CMD_TIMEOUT_MS / 1000}s`));
      }, CMD_TIMEOUT_MS);
      pending.set(i, { res, timer });
      ws.send(JSON.stringify({ id: i, method, params }));
    });
  const evalIn = async (expression) => {
    const r = await cmd("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description || "eval failed");
    return r.result?.result?.value;
  };
  await cmd("Runtime.enable");
  // Outstanding timers are cleared on close, or a 30s timer left armed by an
  // in-flight command keeps the process alive well past the last assertion.
  const close = () => {
    for (const p of pending.values()) clearTimeout(p.timer);
    pending.clear();
    try { ws.close(); } catch { /* already gone */ }
  };
  return { cmd, evalIn, close };
}

// ------------------------------------------------------------------- resh

/// Builds and starts a private resh. `RESH_CMD` is never set: substituting a
/// plain command for dtach is exactly the trap that once let a broken socket
/// directory reach production green (see CLAUDE.md, "The dev/prod
/// substitution trap"). A browser test that skipped real dtach would be
/// testing the same fiction from a different angle.
export async function startResh({ repoRoot, stateDir, roots, port }) {
  const meta = JSON.parse(new TextDecoder().decode(
    (await new Deno.Command("cargo", { args: ["metadata", "--format-version", "1", "--no-deps"], cwd: repoRoot, stdout: "piped" }).output()).stdout,
  ));
  const build = await new Deno.Command("cargo", { args: ["build", "-q"], cwd: repoRoot, stdout: "inherit", stderr: "inherit" }).output();
  if (!build.success) throw new Error("cargo build failed");
  const bin = `${meta.target_directory}/debug/resh`;

  const spawn = () => new Deno.Command(bin, {
    args: [String(port)],
    // clearEnv so nothing from the developer's shell — RESH_CMD above all —
    // leaks in and quietly changes what is under test.
    clearEnv: true,
    env: {
      PATH: Deno.env.get("PATH") ?? "/usr/bin:/bin",
      HOME: Deno.env.get("HOME") ?? "/tmp",
      SHELL: "/bin/bash",
      TERM: "xterm-256color",
      RESH_ROOTS: roots,
      RESH_STATE_DIR: stateDir,
      RESH_STATIC: `${repoRoot}/static`,
      // The one knob deliberately let through the clearEnv above: setting it
      // low turns a run into a soak test for the keepalive ping, so a browser
      // meets hundreds of them instead of two.
      ...(Deno.env.get("RESH_PING_SECS") ? { RESH_PING_SECS: Deno.env.get("RESH_PING_SECS") } : {}),
    },
    stdout: "null", stderr: "null",
  }).spawn();

  let proc = spawn();
  const wait = () => until(async () => {
    try { return (await fetch(`http://127.0.0.1:${port}/`)).status === 200; } catch { return false; }
  }, 30, "resh to listen");
  if (!await wait()) throw new Error("resh never started listening");

  return {
    port,
    /// Kills and restarts the server on the same port and state dir. The
    /// dtach masters are not ours to kill — they daemonise away from this
    /// process — so sessions survive, which is what the deployed unit does
    /// (`KillMode=process`) and what makes a restart worth testing at all.
    restart: async () => {
      try { proc.kill("SIGKILL"); } catch {}
      await proc.status;
      proc = spawn();
      return await wait();
    },
    close: async () => { try { proc.kill("SIGKILL"); } catch {} await proc.status; },
  };
}

// ------------------------------------------------------------------ proxy

/// A TCP proxy the test can sever, standing between the browser and resh.
///
/// This is the only way found to reproduce a sleeping laptop: Chrome's own
/// `Network.emulateNetworkConditions {offline:true}` blocks *new* requests
/// and leaves established sockets open, so an earlier version of this test
/// asserted "it reconnected" while nothing had ever disconnected. Cutting the
/// TCP connection here gives the browser exactly what a sleep gives it — a
/// dead socket with no close frame (1006) — while the server, the session,
/// the shell and the scrollback all carry on untouched.
export function startProxy({ listenPort, upstreamPort }) {
  const pairs = new Set();
  let accepting = true;
  const listener = Deno.listen({ hostname: "127.0.0.1", port: listenPort });
  (async () => {
    for await (const c of listener) {
      if (!accepting) { try { c.close(); } catch {} continue; }
      let up;
      try { up = await Deno.connect({ hostname: "127.0.0.1", port: upstreamPort }); }
      catch { try { c.close(); } catch {} continue; }
      const pair = { c, up };
      pairs.add(pair);
      const pipe = async (a, b) => {
        try { await a.readable.pipeTo(b.writable); } catch { /* the cut itself lands here */ }
        finally { pairs.delete(pair); }
      };
      pipe(c, up);
      pipe(up, c);
    }
  })();
  return {
    cut() {
      accepting = false;
      for (const p of pairs) { try { p.c.close(); } catch {} try { p.up.close(); } catch {} }
      pairs.clear();
    },
    resume() { accepting = true; },
    close() { this.cut(); try { listener.close(); } catch {} },
  };
}

// ---------------------------------------------------------------- fixture

/// A throwaway project for the run. Its own directory and its own state dir
/// mean the sessions this test spawns cannot collide with a real one, and the
/// teardown can identify its own dtach processes by that unique path.
export async function fixture() {
  const base = await Deno.makeTempDir({ prefix: "resh-browser-" });
  const roots = `${base}/roots`;
  const project = `${roots}/proj`;
  await Deno.mkdir(project, { recursive: true });
  await Deno.writeFile(`${project}/hello.md`, enc.encode("# hello\n"));
  await new Deno.Command("git", { args: ["init", "-q"], cwd: project, stdout: "null", stderr: "null" }).output();
  const stateDir = `${base}/state`;
  await Deno.mkdir(stateDir, { recursive: true });
  return {
    base, roots, project: "proj", stateDir,
    cleanup: async () => {
      // The shells this run started are dtach masters holding sockets under
      // our state dir; nothing else on the machine can match that path.
      await killByCmdline(stateDir);
      await sleep(300);
      try { await Deno.remove(base, { recursive: true }); } catch {}
    },
  };
}

/// Where the browser profile goes. Snap-packaged Chromium is confined to
/// non-hidden paths under $HOME — it cannot read /tmp or any dotted
/// directory — so this is neither the repo's usual tmp conventions nor
/// makeTempDir.
export function profileDir(repoRoot) {
  const home = Deno.env.get("HOME") ?? "/tmp";
  return repoRoot.startsWith(home + "/")
    ? `${repoRoot}/tests/browser/tmp/profile`
    : `${home}/resh-browser-tmp/profile`;
}
