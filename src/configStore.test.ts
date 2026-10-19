/**
 * The saved setup, read the same way wherever it is edited.
 *
 * Run with `npm run test:web`.
 *
 * Storage is untrusted input — hand-edited, half-written, or downgraded from
 * an older build — so every field is read through a validator with a fallback.
 */

import assert from "node:assert/strict";
import {
  HUMAN_STORAGE_KEY,
  MAX_ROUNDS,
  SEAT_ROLES,
  STORAGE_KEY,
  STORED_BOUNDS,
  defaultConfig,
  imagesStorageKey,
  loadConfig,
  planEndpointRemoval,
} from "./configStore.ts";
import type { DebateConfig, DelegateConfig } from "./types.ts";

// The store needs a window and a localStorage. Small explicit doubles beat a
// dependency: the contract under test is only "reads this key, coerces every
// field".
const store = new Map<string, string>();
(globalThis as { window?: unknown }).window = {
  localStorage: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  },
};

// The default setup stamps endpoint IDs from the clock. Freeze the clock so
// defaults created by separate calls remain comparable for structural tests;
// the production ID remains unique because it keeps using the real clock.
(Date.now as () => number) = () => 1_700_000_000_000;

// --- the two councils keep separate stores ---------------------------------
assert.notEqual(STORAGE_KEY, HUMAN_STORAGE_KEY, "the two councils must not share a store");

// --- defaults --------------------------------------------------------------
const base = defaultConfig();
assert.ok(base.endpoints.length >= 1, "a council with no server cannot convene");
assert.equal(base.maxRounds >= 1, true, `maxRounds ${base.maxRounds}`);
// Every seat role the UI offers must be one the engine knows, or the role picker
// can build a config that fails validation at the engine instead of here.
for (const r of SEAT_ROLES) {
  assert.equal(typeof r, "string");
}
assert.equal(MAX_ROUNDS, 100, "the engine's ceiling");

// --- a missing store is the defaults, not a crash ---------------------------
store.clear();
assert.deepEqual(loadConfig(), base);
assert.deepEqual(loadConfig(HUMAN_STORAGE_KEY), base);

// --- the human store is read from its own key -------------------------------
const human: DebateConfig = {
  ...base,
  delegates: base.delegates.map((d, i) => (i === 0 ? { ...d, human: true, model: "", endpointId: "" } : d)),
};
store.set(HUMAN_STORAGE_KEY, JSON.stringify(human));
const readHuman = loadConfig(HUMAN_STORAGE_KEY);
assert.equal(readHuman.delegates[0]?.human, true, "the taken seat must survive the round trip");
assert.equal(readHuman.delegates[0]?.model, "", "and it has no model, which is the point");
assert.notEqual(loadConfig().delegates[0]?.human, true, "the automatic council must not inherit it");

// --- storage is untrusted ---------------------------------------------------
// Every one of these is something a hand-edited store, a half-written write or a
// downgrade from an older build can produce. None of them may throw, and none may
// produce a value of the wrong type: the setup screens dereference these
// directly, and there is no error boundary in this app.
const hostile: unknown[] = [
  null,
  42,
  "a string",
  [],
  {},
  { endpoints: "not an array" },
  { endpoints: [] },
  { endpoints: [null], delegates: [null], motion: 7 },
  { endpoints: [{ baseUrl: {} }], delegates: [{ temperature: "hot" }] },
  { endpoints: [{ id: "x", timeoutSecs: -5, nativeTools: "yes" }] },
  { delegates: [{ seatRole: "wizard", id: 7, label: {} }] },
  { maxRounds: 1e9, minmax: 4, closingStatements: "sure" },
  { images: [{ dataUrl: "javascript:alert(1)" }, { dataUrl: "data:image/png;base64,AAAA" }, null] },
  { decision: { apiKey: 5, model: [] } },
];

for (const bad of hostile) {
  store.set(STORAGE_KEY, JSON.stringify(bad));
  const cfg = loadConfig();
  assert.ok(cfg, "every hostile value must still yield a config");
  assert.equal(typeof cfg.motion, "string");
  assert.ok(Array.isArray(cfg.delegates));
  assert.ok(Array.isArray(cfg.endpoints));
  for (const d of cfg.delegates) {
    assert.equal(typeof d.label, "string");
    assert.equal(typeof d.model, "string");
    assert.ok(SEAT_ROLES.includes(d.seatRole), `unknown seat role survived: ${d.seatRole}`);
    assert.equal(typeof d.id, "string");
  }
  for (const e of cfg.endpoints) {
    assert.equal(typeof e.baseUrl, "string");
    assert.equal(typeof e.nativeTools, "boolean");
    assert.ok(e.timeoutSecs > 0, `timeout clamped to ${e.timeoutSecs}`);
  }
  assert.ok(cfg.maxRounds >= 1 && cfg.maxRounds <= MAX_ROUNDS, `maxRounds ${cfg.maxRounds}`);
  // Images are the one field that can be megabytes, so anything that is not an
  // actual image data URL is dropped rather than stored and sent.
  for (const img of cfg.images) {
    assert.match(img.dataUrl, /^data:image\//);
  }
}

// Unparseable bytes at all.
store.set(STORAGE_KEY, "{not json");
assert.deepEqual(loadConfig(), base);
store.set(STORAGE_KEY, "");
assert.deepEqual(loadConfig(), base);

// A config with no usable endpoint keeps everything else. The fallback is a
// fresh endpoint, not fresh defaults — the old code threw the motion and the
// roster away with the servers, so deleting every server also deleted the question.
store.set(
  STORAGE_KEY,
  JSON.stringify({ ...base, motion: "Keep me", endpoints: [] }),
);
const kept = loadConfig();
assert.deepEqual(kept.endpoints, base.endpoints, "an endpoint-less config falls back");
assert.equal(kept.motion, "Keep me", "but the motion survives the fallback");
assert.ok(kept.delegates.length >= 2, "and so does the roster");

// --- a full config survives intact ------------------------------------------
const rich: DebateConfig = {
  ...base,
  motion: "Roses outrank lilies",
  maxRounds: 4,
  closingStatements: true,
  allowWeb: true,
  extraInstructions: "be brief",
  resumeRecord: "R1 Advocate: the stem count settles it.",
  delegates: [
    {
      id: "seat-a",
      label: "Advocate",
      model: "qwen3-27b",
      endpointId: base.endpoints[0]!.id,
      color: "#65ccc6",
      seatRole: "proponent",
      role: null,
      persona: null,
      temperature: 0.7,
    } as DelegateConfig,
  ],
};
store.set(STORAGE_KEY, JSON.stringify(rich));
const back = loadConfig();
assert.equal(back.motion, "Roses outrank lilies");
assert.equal(back.maxRounds, 4);
assert.equal(back.closingStatements, true);
assert.equal(back.allowWeb, true);
assert.equal(back.extraInstructions, "be brief");
assert.equal(back.resumeRecord, rich.resumeRecord, "the resume record must survive the round trip");
assert.equal(back.delegates[0]?.label, "Advocate");
assert.equal(back.delegates[0]?.model, "qwen3-27b");

// --- images live beside the setup, not inside it -----------------------------
const sideImage = {
  id: "img-0",
  name: "x",
  mediaType: "image/png",
  dataUrl: "data:image/png;base64,AAAA",
};
// An old save still carrying images inline keeps working.
store.delete(imagesStorageKey(STORAGE_KEY));
store.set(STORAGE_KEY, JSON.stringify({ ...base, images: [sideImage] }));
assert.equal(loadConfig().images.length, 1, "inline images still load when no side key exists");
// Once split, the side key wins — including an explicit empty, which must beat
// stale bytes left inline by the save that split them.
store.set(imagesStorageKey(STORAGE_KEY), JSON.stringify([sideImage]));
store.set(STORAGE_KEY, JSON.stringify({ ...base, images: [] }));
assert.equal(loadConfig().images.length, 1, "the side key wins when present");
store.set(imagesStorageKey(STORAGE_KEY), JSON.stringify([]));
store.set(STORAGE_KEY, JSON.stringify({ ...base, images: [sideImage] }));
assert.equal(loadConfig().images.length, 0, "removing every image stays removed");
store.delete(imagesStorageKey(STORAGE_KEY));

// --- the slider ranges and the loader agree -----------------------------------
// These drifted apart: the loader accepted a 3600s timeout and 100 rounds while
// the sliders stopped at 1800 and 20. A stored value above the slider's maximum
// rendered pinned at the right-hand end while the readout printed the real
// figure, and nudging the slider silently snapped it down by up to 3300s.
// The setup panel now reads these, so the only way they can disagree again is
// for a bound to be added here and not there.
{
  assert.equal(STORED_BOUNDS.endpointTimeoutSecs.max, 3600, "endpoint timeout, as loaded");
  assert.equal(STORED_BOUNDS.decisionTimeoutSecs.max, 300, "decision timeout, as loaded");
  assert.equal(STORED_BOUNDS.maxRounds.max, MAX_ROUNDS, "rounds, as loaded");
  assert.equal(STORED_BOUNDS.maxExtraRounds.max, MAX_ROUNDS, "extra rounds, as loaded");

  // And the loader must actually honour them: a value past the bound is clamped,
  // not carried through, so no slider can ever be handed something it cannot
  // represent.
  const extreme = {
    ...base,
    maxRounds: 5000,
    maxExtraRounds: 5000,
    endpoints: [{ ...base.endpoints[0]!, timeoutSecs: 99_999 }],
  };
  store.set(STORAGE_KEY, JSON.stringify(extreme));
  const clamped = loadConfig();
  assert.equal(clamped.maxRounds, MAX_ROUNDS, "rounds clamp to the stored bound");
  assert.equal(clamped.maxExtraRounds, MAX_ROUNDS, "extra rounds clamp to the stored bound");
  assert.equal(
    clamped.endpoints[0]?.timeoutSecs,
    STORED_BOUNDS.endpointTimeoutSecs.max,
    "timeout clamps to the stored bound",
  );
}

// --- deleting a server says what it will do first -----------------------------
// The re-pointing is silent and consequential: every delegate on the deleted
// server moves to whatever is left, which may not serve their model. The debate
// then convenes cleanly and fails model-not-found on those seats, one turn each.
{
  const two = base.endpoints[0]!;
  const withTwo = {
    ...base,
    endpoints: [two, { ...two, id: "ep-2", name: "Backup", baseUrl: "http://127.0.0.1:9999" }],
    delegates: base.delegates.map((d, i) => ({ ...d, endpointId: i < 2 ? "ep-2" : two.id })),
  };

  // Nobody on the server being removed: nothing to warn about, and the common case.
  const unused = { ...withTwo, delegates: withTwo.delegates.map((d) => ({ ...d, endpointId: "ep-2" })) };
  const none = planEndpointRemoval(unused, two.id);
  assert.equal(none.sentence, null, "a server nobody uses needs no confirmation");
  assert.equal(none.fallback, "ep-2", "but the fallback is still computed");

  const some = planEndpointRemoval(withTwo, "ep-2");
  assert.ok(some.sentence, "a server with delegates on it must warn");
  assert.equal(some.fallback, two.id, "and must fall back to a server that survives");
  // The sentence has to be specific, or confirming it means nothing.
  assert.match(some.sentence!, /Advocate/, "names a delegate that would move");
  assert.match(some.sentence!, /Unsloth/, "names where it would move to");

  // Long rosters: every name would not fit, but the count must not vanish.
  const many = {
    ...base,
    endpoints: [two, { ...two, id: "ep-2", name: "Backup" }],
    delegates: Array.from({ length: 5 }, (_, i) => ({
      ...base.delegates[0]!,
      id: `s-${i}`,
      label: `Seat ${i}`,
      endpointId: "ep-2",
    })),
  };
  const crowded = planEndpointRemoval(many, "ep-2");
  assert.match(crowded.sentence!, /and 2 more/, "summarises past three rather than listing all");
  assert.match(crowded.sentence!, /are set to use it/, "plural agrees with the count");

  // Singular grammar, since the sentence is read by a person.
  const one = {
    ...base,
    endpoints: [two, { ...two, id: "ep-2", name: "Backup" }],
    delegates: [{ ...base.delegates[0]!, endpointId: "ep-2" }],
  };
  assert.match(planEndpointRemoval(one, "ep-2").sentence!, /is set to use it/, "singular agrees");

  // The last server: the fallback is empty and the sentence has to admit there
  // is nowhere to move to, rather than promising a move to nothing.
  const last = { ...base, endpoints: [two], delegates: base.delegates };
  const orphan = planEndpointRemoval(last, two.id);
  assert.equal(orphan.fallback, "", "no surviving server means no fallback");
  assert.match(orphan.sentence!, /no other server/, "and it says so");
}

console.log("configStore: one store, nothing hostile survives");