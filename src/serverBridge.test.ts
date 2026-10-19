/**
 * The served transport's secret hygiene and stream ordering.
 *
 * Run with `npm run test:web`. Two rules shape this module: keys never leave
 * the browser, and a reconnect that replays the tail must not double-speak
 * turns already on the transcript. Both are pure functions, so both are
 * pinned here rather than discovered on a phone.
 */

import assert from "node:assert/strict";
import {
  configHasSecrets,
  createSeqTracker,
  injectNote,
  isHealthBody,
  stripSecrets,
  subscribe,
  wakeSubscription,
} from "./serverBridge.ts";
import type { DebateConfig, EngineEvent } from "./types.ts";

function config(): DebateConfig {
  return {
    motion: "test",
    extraInstructions: null,
    endpoints: [
      {
        id: "ep-1",
        name: "Local",
        provider: "openaiCompatible",
        baseUrl: "http://127.0.0.1:8888",
        apiKey: "sk-live",
        headers: {},
        timeoutSecs: 300,
        reasoningWire: "serverDefault",
        reasoningEffort: null,
        thinking: null,
        nativeTools: true,
        stream: true,
        allowInvalidCerts: false,
        includeUsage: true,
      },
    ],
    delegates: [],
    maxRounds: 3,
    autoAdjourn: false,
    closingStatements: true,
    decision: { provider: "seat", baseUrl: "", apiKey: "k", model: "", timeoutSecs: 30 },
    maxExtraRounds: 0,
    allowWeb: false,
    maxFetchesPerTurn: 3,
    maxFetchChars: 6000,
    images: [],
  };
}

const full = config();
assert.equal(configHasSecrets(full), true);

const stripped = stripSecrets(full);
// The original is untouched: callers keep editing their own copy.
assert.equal(full.endpoints[0]?.apiKey, "sk-live");
assert.equal(stripped.endpoints[0]?.apiKey, null);
assert.equal(stripped.decision.apiKey, null);
assert.equal(configHasSecrets(stripped), false);
// Everything else rides along unchanged.
assert.equal(stripped.motion, "test");
assert.equal(stripped.endpoints[0]?.baseUrl, "http://127.0.0.1:8888");

const seen = createSeqTracker();
assert.equal(seen.accept(0), true);
assert.equal(seen.accept(0), false);
assert.equal(seen.accept(1), true);
assert.equal(seen.accept(1), false);
// A stale or garbage frame never moves the watermark backwards.
assert.equal(seen.accept(0), false);
assert.equal(seen.accept("2"), false);
assert.equal(seen.accept(Number.NaN), false);
assert.equal(seen.accept(2), true);

// A dev server's SPA fallback answers every path with 200 HTML. Status
// alone once convinced the desktop app a local server was present; only the
// payload counts.
assert.equal(isHealthBody({ ok: true, service: "council-serve", version: "0.1.0" }), true);
assert.equal(isHealthBody({ ok: true }), false);
assert.equal(isHealthBody("<!doctype html>"), false);
assert.equal(isHealthBody(null), false);

// --- the stream follows debates and revives dead sockets ----------------------
//
// A fake EventSource plus a controllable `/api/debates/active`: the two ways a
// live debate used to go silent were a second convene inheriting the first
// debate's watermark (every early event dropped) and a killed socket with the
// same debate id never re-attaching (frozen transcript, no error).
class FakeSource {
  static instances: FakeSource[] = [];
  static CLOSED = 2;
  onmessage: ((m: { data: string }) => void) | null = null;
  readyState = 1;
  closed = false;
  url: string;
  constructor(url: string) {
    this.url = url;
    FakeSource.instances.push(this);
  }
  close() {
    this.closed = true;
    this.readyState = 2;
  }
}
(globalThis as { EventSource?: unknown }).EventSource = FakeSource;

let active: string | null = "deb-A";
(globalThis as { fetch?: unknown }).fetch = async () => ({
  ok: true,
  json: async () => ({ id: active }),
});
(globalThis as { window?: unknown }).window = { dispatchEvent: () => {} };

const got: EngineEvent[] = [];
const stop = await subscribe((e) => {
  got.push(e);
});
const settle = async () => {
  for (let i = 0; i < 20; i += 1) await new Promise((r) => setTimeout(r, 0));
};
const frame = (seq: number) =>
  JSON.stringify({ seq, event: { type: "notice", text: `n${seq}`, tone: "info" } });

wakeSubscription();
await settle();
assert.equal(FakeSource.instances.length, 1, "the stream attaches on wake");
const srcA = FakeSource.instances[0]!;
assert.ok(srcA.url.includes("deb-A"), "to the running debate");
srcA.onmessage!({ data: frame(5) });
assert.equal(got.length, 1, "its events arrive");

// A second debate restarts numbering at zero: carrying the old watermark over
// would drop everything until the new numbering caught up — the whole debate.
active = "deb-B";
wakeSubscription();
await settle();
assert.equal(FakeSource.instances.length, 2, "a new debate re-attaches");
const srcB = FakeSource.instances[1]!;
assert.ok(srcB.url.includes("deb-B"), "to the new debate, not the grave");
srcB.onmessage!({ data: frame(0) });
srcB.onmessage!({ data: frame(1) });
assert.equal(got.length, 3, "a new debate restarts numbering — a carried watermark would swallow these");

// The socket dies without the debate changing (sleep, killed connection): the
// poll sees the same id and must re-attach rather than watch a closed socket.
srcB.readyState = FakeSource.CLOSED;
wakeSubscription();
await settle();
assert.equal(FakeSource.instances.length, 3, "a dead socket re-attaches without a new debate");
assert.ok(FakeSource.instances[2]!.url.includes("deb-B"), "to the same debate");
stop();

// `injectNote` reports delivery, because the composer clears its box on the
// answer. It used to return void and return quietly with no debate, so a note
// typed before Convene vanished while looking delivered — and the operator's
// next turn then argued against a point no model had seen.
{
  let postStatus = 200;
  (globalThis as { fetch?: unknown }).fetch = async (url: string) => {
    if (String(url).includes("/inject")) return { ok: postStatus < 400, status: postStatus };
    return { ok: true, json: async () => ({ id: active }) };
  };
  assert.equal(await injectNote("a point for the room"), true, "a running debate accepts the note");

  // The server has forgotten it: 404, not an error. Reported as not delivered.
  postStatus = 404;
  assert.equal(
    await injectNote("after the server forgot"),
    false,
    "a forgotten debate reports the note as undelivered rather than throwing",
  );
  assert.equal(
    await injectNote("and again"),
    false,
    "the dropped id is not cached, so the next note re-asks instead of trusting a stale one",
  );

  // Genuinely no debate at all.
  active = null;
  assert.equal(await injectNote("before Convene"), false, "no debate means no delivery");
}
active = "deb-A";
(globalThis as { fetch?: unknown }).fetch = async () => ({
  ok: true,
  json: async () => ({ id: active }),
});

console.log("serverBridge: secrets stripped, stream order pinned, note delivery reported");
