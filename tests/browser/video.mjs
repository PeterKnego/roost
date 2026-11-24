//! A video (a screencast, typically) opens in the browser's own player.
//!
//! The route's headers, the range arithmetic, the refusals and the fragment's
//! escaping are covered in `src/routes.rs`. What only a browser can say is
//! whether the **player actually decodes it and can seek** — a `<video>`
//! element existing proves nothing, since one pointed at a 404 is still a
//! `<video>`, and one served without range support loads fine and then
//! cannot jump anywhere it has not downloaded.
//!
//! The fixtures are made by ffmpeg (the test skips without it): a VP9 WebM,
//! which every Chromium build decodes, and an H.264 MP4, which Chrome does
//! but an open-source Chromium may not — so the MP4 section asks the browser
//! first and says so when it skips.
//!
//! Revert-checks performed, all restored:
//!   (a) dropping the video extensions from `RENDERED_EXT` in app.js ->
//!       section A fails on `defaultMode`.
//!   (b) answering every request with the whole file (`byte_range` always
//!       `Whole`) -> section B fails with a 200. Section A's seek still
//!       passes then: Chromium buffers a 60 KB fixture whole and seeks in
//!       memory. A real screencast is far past what it buffers, which is
//!       the case B's 206 stands for.
//!   (c) dropping the `.content > .vidview` rule -> section A fails on the
//!       fill check: the player shrinks to its aspect-ratio height. (The
//!       phone-width check passes without it — the flex column's stretch
//!       is what holds the width.)
//!
//! Run: deno run -A tests/browser/video.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

async function ffmpeg(args) {
  try {
    const r = await new Deno.Command("ffmpeg", { args: ["-hide_banner", "-loglevel", "error", "-y", ...args],
      stdout: "null", stderr: "piped" }).output();
    return r.success;
  } catch {
    return false;
  }
}

const fx = await fixture();
const src = ["-f", "lavfi", "-i", "testsrc=size=640x360:rate=25", "-t", "4"];
const madeWebm = await ffmpeg([...src, "-c:v", "libvpx-vp9", "-b:v", "200k", `${fx.dir}/cast.webm`]);
if (!madeWebm) {
  console.log("SKIP: no ffmpeg with libvpx-vp9 to make the fixture");
  await fx.cleanup();
  Deno.exit(0);
}
const madeMp4 = await ffmpeg([...src, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
  `${fx.dir}/cast.mp4`]);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

/// Opens `rel` from the tree and waits for its player to have metadata.
async function openVideo(evalIn, rel) {
  await evalIn(`document.querySelector('a[data-rel="${rel}"]').click(); true`);
  await until(() => evalIn(`!!document.querySelector('.content > video.vidview[src*="${rel}"]')`), 20, "the player");
  return until(() => evalIn(`(document.querySelector('.content > video.vidview[src*="${rel}"]')?.readyState ?? 0) >= 1`),
    20, `${rel} metadata`);
}

/// Seeks the player for `rel` and reports where it settled once it can play
/// there. Muted and paused, so nothing depends on autoplay policy.
const seekTo = (evalIn, rel, t) => evalIn(`new Promise((done) => {
  const v = document.querySelector('.content > video.vidview[src*="${rel}"]');
  v.muted = true;
  const settle = () => done(v.currentTime);
  v.addEventListener("seeked", () => setTimeout(settle, 300), { once: true });
  setTimeout(settle, 8000);
  v.currentTime = ${t};
})`);

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");
  await until(() => evalIn(`!!document.querySelector('a[data-rel="cast.webm"]')`), 20, "tree");

  console.log("A. clicking a WebM in the tree plays it, and it can seek");
  ok(await evalIn(`defaultMode("cast.webm") === "Preview" && defaultMode("cast.mp4") === "Preview"
    && defaultMode("cast.mov") === "Preview"`), "a video opens in Preview");
  ok(await openVideo(evalIn, "cast.webm"), "the player read the WebM's metadata");
  const meta = JSON.parse(await evalIn(`JSON.stringify((() => {
    const v = document.querySelector('.content > video.vidview');
    return { d: v.duration, w: v.videoWidth, h: v.videoHeight };
  })())`));
  ok(Math.abs(meta.d - 4) < 0.5 && meta.w === 640 && meta.h === 360,
     `and decoded it: ${meta.w}x${meta.h}, ${meta.d.toFixed(2)} s`);
  const fill = JSON.parse(await evalIn(`JSON.stringify((() => {
    const v = document.querySelector('.content > video.vidview');
    return { v: v.getBoundingClientRect().height, c: v.closest('.content').clientHeight };
  })())`));
  ok(fill.v > fill.c - 120, `the player fills the pane under the path (${Math.round(fill.v)} of ${fill.c}px)`);
  const at = await seekTo(evalIn, "cast.webm", 3.0);
  ok(Math.abs(at - 3.0) < 0.3, `a seek to 3.0 s lands there (${at})`);
  ok(await evalIn(`!document.querySelector('.path .modebtn')`), "no ✎: a video has no text to edit");

  console.log("\nB. the raw route answers a range with 206");
  const href = await evalIn(`document.querySelector(".content > a.vidopen").getAttribute("href")`);
  ok(/^\/frag\/[^?]+\/raw\?path=cast\.webm&v=\d+$/.test(href), `the link is the raw route (${href})`);
  const r = JSON.parse(await evalIn(`fetch(${JSON.stringify(href)}, { headers: { Range: "bytes=10-19" } })
    .then(async (r) => JSON.stringify({ s: r.status, cr: r.headers.get("content-range"), n: (await r.arrayBuffer()).byteLength }))`));
  ok(r.s === 206 && /^bytes 10-19\/\d+$/.test(r.cr) && r.n === 10, `206 with ten bytes (${JSON.stringify(r)})`);

  console.log("\nC. an H.264 MP4");
  const h264 = await evalIn(`document.createElement("video").canPlayType('video/mp4; codecs="avc1.42E01E"')`);
  if (!madeMp4) {
    console.log("  skip  ffmpeg has no libx264");
  } else if (!h264) {
    console.log("  skip  this Chromium build has no H.264 decoder (Chrome and Safari do)");
  } else {
    ok(await openVideo(evalIn, "cast.mp4"), "the player read the MP4's metadata");
    const at4 = await seekTo(evalIn, "cast.mp4", 2.0);
    ok(Math.abs(at4 - 2.0) < 0.3, `a seek to 2.0 s lands there (${at4})`);
  }

  console.log("\nD. on a phone the player fits the screen");
  await cmd("Emulation.setDeviceMetricsOverride", { width: 390, height: 800, deviceScaleFactor: 2, mobile: true });
  await evalIn(`revealPane(Number(document.querySelector(".content > video.vidview").closest(".pane").dataset.pane)); true`);
  await sleep(300);
  const phone = JSON.parse(await evalIn(`JSON.stringify((() => {
    const v = document.querySelector(".content > video.vidview").getBoundingClientRect();
    return { w: v.width, right: v.right, inline: document.querySelector(".content > video.vidview").hasAttribute("playsinline"),
      link: document.querySelector(".content > a.vidopen").getBoundingClientRect().height };
  })())`));
  ok(phone.w > 0 && phone.right <= 391, `the player is within the 390px screen (${phone.w}px wide)`);
  ok(phone.inline, "and plays inline rather than taking over the screen on iOS");
  ok(phone.link >= 44, `the link is a 44px tap target (${phone.link})`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
