//! A real Firefox for the browser suite, driven over Marionette.
//!
//! Not CDP, which Firefox no longer speaks, and not WebDriver BiDi, which can
//! install an extension but refuses to open or script any moz-extension page.
//! Marionette's privileged ("chrome") context is how Firefox's own extension
//! tests reach in: it is what grants an optional permission and writes an
//! extension's storage without a person clicking through Firefox's own
//! dialogs. That needs `--remote-allow-system-access`, which nothing outside
//! a test profile should ever be started with.
//!
//! The browser is found, never installed: `$FIREFOX`, else `firefox` on PATH.
//! Without one the caller skips, as the Chromium tests do.
import { freePort, sleep, until } from "./harness.mjs";

export async function findFirefox() {
  if (Deno.env.get("FIREFOX")) return Deno.env.get("FIREFOX");
  const p = new Deno.Command("sh", { args: ["-c", "command -v firefox"], stdout: "piped" });
  const { code, stdout } = await p.output();
  return code === 0 ? new TextDecoder().decode(stdout).trim() : null;
}

/// Starts Firefox on a fresh profile and returns a Marionette client.
/// `uuids` pins moz-extension hostnames, so a test can name an extension's
/// pages; optional permission prompts are switched off, the test-profile
/// preference Firefox's own suites use.
export async function startFirefox({ uuids = {} } = {}) {
  const bin = await findFirefox();
  if (!bin) {
    console.log("SKIP: no firefox found. Set FIREFOX=/path/to/firefox.");
    Deno.exit(0);
  }
  const port = await freePort();
  const profile = await Deno.makeTempDir({ prefix: "roost-firefox-" });
  await Deno.writeTextFile(`${profile}/user.js`, [
    `user_pref("marionette.port", ${port});`,
    `user_pref("extensions.webextensions.uuids", ${JSON.stringify(JSON.stringify(uuids))});`,
    `user_pref("extensions.webextOptionalPermissionPrompts", false);`,
    `user_pref("browser.shell.checkDefaultBrowser", false);`,
    `user_pref("datareporting.policy.dataSubmissionEnabled", false);`,
    `user_pref("browser.aboutwelcome.enabled", false);`,
  ].join("\n") + "\n");
  const proc = new Deno.Command(bin, {
    args: ["--headless", "--no-remote", "--profile", profile, "--marionette", "--remote-allow-system-access"],
    stdout: "null", stderr: "null",
  }).spawn();

  let conn = null;
  await until(async () => { conn = await Deno.connect({ hostname: "127.0.0.1", port }); return true; }, 30, "firefox's marionette port");
  if (!conn) { try { proc.kill("SIGKILL"); } catch {} throw new Error(`${bin} never opened its marionette port`); }

  // Marionette frames are `<byte length>:<json>`; replies are [1, id, error, result].
  let buf = new Uint8Array(0);
  const waiting = new Map();
  (async () => {
    const chunk = new Uint8Array(1 << 16);
    for (;;) {
      const n = await conn.read(chunk).catch(() => null);
      if (n === null) break;
      const nb = new Uint8Array(buf.length + n);
      nb.set(buf); nb.set(chunk.subarray(0, n), buf.length); buf = nb;
      for (;;) {
        const colon = buf.indexOf(58);
        if (colon < 0) break;
        const len = Number(new TextDecoder().decode(buf.subarray(0, colon)));
        if (buf.length < colon + 1 + len) break;
        const msg = JSON.parse(new TextDecoder().decode(buf.subarray(colon + 1, colon + 1 + len)));
        buf = buf.subarray(colon + 1 + len);
        if (!Array.isArray(msg)) continue; // the greeting
        const [, id, err, res] = msg;
        const w = waiting.get(id);
        waiting.delete(id);
        if (w) w(err, res);
      }
    }
  })();
  let id = 0;
  let context = "content";
  const cmd = (name, params = {}) => new Promise((resolve, reject) => {
    const i = ++id;
    const timer = setTimeout(() => { waiting.delete(i); reject(new Error(`marionette ${name} got no reply in 60s`)); }, 60_000);
    waiting.set(i, (err, res) => {
      clearTimeout(timer);
      if (err) reject(new Error(`${name}: ${err.message || err.error}`));
      else resolve(res && "value" in res ? res.value : res);
    });
    const body = new TextEncoder().encode(JSON.stringify([0, i, name, params]));
    conn.write(new Uint8Array([...new TextEncoder().encode(`${body.length}:`), ...body]));
  });
  const setContext = async (c) => { if (c !== context) { await cmd("Marionette:SetContext", { value: c }); context = c; } };
  await sleep(300);
  await cmd("WebDriver:NewSession", { capabilities: {} });

  return {
    cmd,
    /// An async script in the privileged context: `done(value)` answers.
    chrome: async (body, args = []) => {
      await setContext("chrome");
      return cmd("WebDriver:ExecuteAsyncScript", {
        script: `const done = arguments[arguments.length - 1]; (async () => { ${body} })().then(done, (e) => done({ error: String(e) }));`,
        args,
      });
    },
    /// An expression in the current web page.
    page: async (expr) => {
      await setContext("content");
      return cmd("WebDriver:ExecuteAsyncScript", {
        script: `const done = arguments[arguments.length - 1]; Promise.resolve(${expr}).then(done, (e) => done({ error: String(e) }));`,
        args: [],
      });
    },
    navigate: async (url) => { await setContext("content"); return cmd("WebDriver:Navigate", { url }); },
    installTemporary: (path) => cmd("Addon:Install", { path, temporary: true }),
    close: async () => {
      try { conn.close(); } catch { /* already gone */ }
      try { proc.kill("SIGKILL"); } catch { /* already gone */ }
      await proc.status;
      await Deno.remove(profile, { recursive: true }).catch(() => {});
    },
  };
}
