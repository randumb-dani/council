/**
 * Council service worker: an installable shell, not an offline engine.
 *
 * The app shell (HTML, JS, CSS, icons) is cached so the PWA installs and
 * starts fast. Everything under `/api/` — debates, snapshots, and especially
 * the event stream — is never cached and never intercepted: a cached event
 * stream would replay a dead debate, and a cached snapshot would show a
 * finished board as live. Installing the app does not remove the other
 * requirement: the local server must be running on this machine.
 *
 * The cache has to be pruned, and the reason is arithmetic. Vite emits
 * content-hashed asset filenames, so every deploy adds a new
 * `index-<hash>.js` and `index-<hash>.css` and nothing ever asks for the old
 * ones again. Caching on arrival and evicting only by cache *name* — which is
 * all this used to do — left every previous deploy's assets in place forever:
 * roughly 660KB per deploy, growing without limit, on a device that probably
 * has a few hundred megabytes to spare. Correctness was never at risk (a stale
 * hash is never referenced), so it failed silently and invisibly.
 *
 * Two things keep it honest, on activate and as a backstop:
 *
 * - **Prune to what the shell references.** Fetch the current `index.html`,
 *   read the assets it actually names, and delete every other `/assets/`
 *   entry. That is self-correcting across any number of deploys and needs no
 *   version string to be kept in step by hand — a forgotten bump is exactly how
 *   the old scheme went wrong.
 * - **Cap the entries.** A belt-and-braces bound, so a pathological number of
 *   hashed files cannot fill the origin quota even if pruning fails.
 */

const CACHE = "council-v2";
const SHELL = ["/", "/index.html", "/manifest.webmanifest"];
/** Most entries one cache may hold before the oldest go. */
const MAX_ENTRIES = 40;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      // Deliberately no `skipWaiting()`. This app holds a running debate — a
      // long stream of SSE frames and a form on screen that must not be swapped
      // underneath the operator. Taking over mid-debate means the tab's already
      // loaded code is now controlled by a worker serving a *different* build,
      // and a lazily imported chunk (the Tauri dialog and file writers, used
      // by the export buttons) can resolve against the new build's asset
      // manifest and fail. Waiting costs a reload of every open tab before an
      // update lands, which is the ordinary PWA bargain and the honest one for
      // an app mid-debate.
      .catch(() => undefined),
  );
});

/** The `/assets/…` URLs the current shell actually references. */
async function liveAssets() {
  try {
    // `index.html` from the network: the one in the cache is this worker's own
    // precache, which may predate the deploy that triggered this activation.
    const res = await fetch("/index.html", { cache: "no-store" });
    const html = await res.text();
    const out = new Set();
    for (const m of html.matchAll(/(?:src|href)="(\/assets\/[^"]+)"/g)) {
      if (m[1]) out.add(new URL(m[1], self.location.origin).pathname);
    }
    return out;
  } catch {
    // Cannot tell which assets are live. Return null rather than an empty set:
    // an empty set would read as "nothing is referenced" and delete everything.
    return null;
  }
}

/**
 * Drop this cache's superseded assets and, failing that, its oldest entries.
 *
 * Only `/assets/` is ever deleted. The shell entries are few and pinned, and
 * the offline navigation fallback depends on `/index.html` being there.
 */
async function trim() {
  const cache = await caches.open(CACHE);
  const keys = await cache.keys();
  const keep = await liveAssets();

  if (keep) {
    const stale = keys.filter((req) => {
      const path = new URL(req.url).pathname;
      if (!path.startsWith("/assets/")) return false;
      return !keep.has(path);
    });
    await Promise.all(stale.map((req) => cache.delete(req)));
    if (stale.length) return;
  }

  // No live list, or nothing was stale: bound the cache anyway.
  if (keys.length <= MAX_ENTRIES) return;
  for (const req of keys.slice(0, keys.length - MAX_ENTRIES)) {
    await cache.delete(req);
  }
}

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))),
      )
      // After the old caches are gone, so the quota is already back before the
      // trim measures anything.
      .then(trim)
      .then(() => self.clients.claim())
      .catch(() => undefined),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  // API and the event stream always go to the network. No exceptions: the
  // request that starts a debate must reach the engine, not a cache.
  if (url.pathname.startsWith("/api/")) return;
  if (event.request.method !== "GET") return;

  event.respondWith(
    caches.match(event.request).then((hit) => {
      if (hit) return hit;
      return fetch(event.request).then((res) => {
        // Cache same-origin app assets as they arrive; navigations fall back
        // to the shell so client routes survive a reload.
        if (res.ok && url.origin === self.location.origin) {
          const copy = res.clone();
          caches
            .open(CACHE)
            .then((cache) => cache.put(event.request, copy))
            .catch(() => undefined);
        }
        return res;
      }).catch(() => {
        if (event.request.mode === "navigate") {
          return caches.match("/index.html");
        }
        throw new Error("offline");
      });
    }),
  );
});