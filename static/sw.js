// Shows OS notifications and routes clicks back to the right window.
//
// The page could call new Notification() directly and skip this file
// entirely — the reason it does not is that a service worker is the only
// thing that can later receive a Web Push message with no tab open. Doing it
// here now means adding push is a `push` listener, not a rewrite.

// Without claim(), a freshly registered worker does not control the page
// until the next reload, and clients.matchAll would find nothing to focus.
self.addEventListener("install", (e) => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  const m = e.data;
  if (!m || m.kind !== "notify") return;
  const n = m.notice;
  // Guard the payload even though only first-party page code posts here: an
  // undefined `notice` would throw a TypeError inside the worker, and an
  // uncaught throw in this context is invisible from the page.
  if (!n || !n.title) return;
  // waitUntil because showNotification is async and this is an
  // ExtendableMessageEvent: without it the browser may terminate the worker
  // between this dispatch and the notification actually appearing, and the
  // notice is lost with nothing to show for it.
  e.waitUntil(
    // The design's threat model renders attribution "from those fields
    // only" — project/session, which the server derived from the PTY pump's
    // own identity, never from the payload. The in-page panel already does
    // this (a separate "notice-who" span); putting project/session in the
    // *title* here — an OS notification's most prominent field — is what
    // keeps a hostile payload (e.g. a `cat`ted file containing a forged OSC
    // sequence) from producing a banner indistinguishable from a real one
    // from another project. n.title/n.body are payload text and go in the
    // body only, never the attribution line.
    self.registration.showNotification(`${n.project} · ${n.session}`, {
      body: `${n.title} — ${n.body}`,
      // One notification per session: a chatty session replaces its own
      // rather than stacking twenty.
      tag: `${n.project}/${n.session}`,
      data: { project: n.project, session: n.session },
      renotify: true,
    })
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const { project, session } = e.notification.data || {};
  // Deliberate bail-out with no waitUntil: there is nothing to focus or
  // navigate to, and close() above already ran synchronously.
  if (!project) return;
  // Same segment-wise encoding as app.js's projectPath, and for the same
  // reason: a project name is a directory name, so it may contain characters
  // that are structural in a URL. `/` stays literal because a nested project
  // like `karpie/src` really is a multi-segment path.
  const target =
    "/" + project.split("/").map(encodeURIComponent).join("/") +
    "#session=" + encodeURIComponent(session || "");
  e.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
      // Prefer a window already on this project: focusing it needs no
      // navigation, so nothing in that tab is disturbed.
      const onProject = wins.find((c) => {
        // Compare decoded: the browser hands back a percent-encoded pathname,
        // so a raw `/${project}` would never match precisely those names that
        // needed encoding in the first place.
        try { return decodeURIComponent(new URL(c.url).pathname) === `/${project}`; } catch { return false; }
      });
      if (onProject) {
        return onProject.focus().then((c) => {
          (c || onProject).postMessage({ kind: "focus", project, session });
        });
      }
      // Otherwise reuse any deadlight window rather than opening a new tab.
      if (wins.length) {
        return wins[0].focus().then((c) => (c || wins[0]).navigate(target));
      }
      return self.clients.openWindow(target);
    })
  );
});
