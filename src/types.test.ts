/**
 * Provider defaults.
 *
 * The engine's `Provider` enum and these tables are written in two languages,
 * so the risk is not that either is wrong but that they disagree — a `str` that
 * parses to `openaiCompatible` while the picker offers `openAiCompatible` fails
 * silently, and the user gets a provider that was never selected.
 */
import assert from "node:assert/strict";
import {
  PROVIDER_BASE_URL,
  PROVIDER_EFFORTS,
  PROVIDER_LABEL,
  canBeTakenByOperator,
  type Provider,
} from "./types.ts";

const PROVIDERS: Provider[] = [
  "openaiCompatible",
  "gemini",
  "deepseek",
  "openrouter",
  "anthropic",
];

const tests: [string, () => void][] = [
  [
    "only the debating roles can be taken by the operator",
    () => {
      // The same rule the engine enforces on a taken seat, pinned on this side
      // so the panel cannot offer a role the engine will refuse at convene time.
      for (const role of ["proponent", "opponent", "rationale", "custom"] as const) {
        assert.ok(canBeTakenByOperator(role), `${role} should be takeable`);
      }
      for (const role of ["judge", "summarizer"] as const) {
        assert.equal(
          canBeTakenByOperator(role),
          false,
          `${role} must stay with the models — a taken one is refused at convene`,
        );
      }
      for (const junk of ["", "Proponent", null, undefined, 3, {}]) {
        assert.equal(canBeTakenByOperator(junk), false, `${String(junk)} should be refused`);
      }
    },
  ],
  [
    "every role is either takeable or deliberately kept",
    () => {
      // The complement is exactly the two that reason for being kept, so a new
      // role cannot silently become untakeable.
      const kept = (["proponent", "opponent", "judge", "rationale", "summarizer", "custom"] as const)
        .filter((r) => !canBeTakenByOperator(r));
      assert.deepEqual([...kept].sort(), ["judge", "summarizer"]);
    },
  ],
  [
    "every provider has a name, an address and a set of levels",
    () => {
      for (const p of PROVIDERS) {
        assert.ok(PROVIDER_LABEL[p]?.trim(), `${p} has no label`);
        // An https address for a hosted service: a provider reached over plain
        // http would put an API key on the wire in the clear. Loopback is the
        // deliberate exception, pinned by the test below.
        const url = PROVIDER_BASE_URL[p];
        const loopback = /^http:\/\/(127\.0\.0\.1|localhost)/.test(url);
        assert.ok(
          loopback || /^https:\/\//.test(url),
          `${p} base URL is not https: ${url}`,
        );
        assert.ok(
          PROVIDER_EFFORTS[p].length >= 2,
          `${p} offers ${PROVIDER_EFFORTS[p].length} level(s), which cannot be chosen between`,
        );
      }
    },
  ],
  [
    "only the local default is not https",
    () => {
      // The exception is deliberate and is the original case: a server on this
      // machine. Everything else is someone else's machine.
      assert.match(PROVIDER_BASE_URL.openaiCompatible, /^http:\/\/127\.0\.0\.1/);
    },
  ],
  [
    "gemini does not offer OpenAI's level names",
    () => {
      // The whole reason Gemini is a separate branch in `build_body`: it takes
      // `thinking_config.thinking_level`, and an OpenAI-shaped value is a 400
      // rather than something ignored.
      assert.ok(!PROVIDER_EFFORTS.gemini.includes("xhigh"));
      assert.ok(PROVIDER_EFFORTS.gemini.includes("minimal"));
    },
  ],
  [
    "anthropic reserves effort names for a later mapping",
    () => {
      // The engine deliberately sends no thinking controls to Anthropic in this
      // release, so these entries document the future vocabulary rather than an
      // active request shape.
      assert.deepEqual([...PROVIDER_EFFORTS.anthropic], ["low", "medium", "high", "max"]);
    },
  ],
  [
    "deepseek offers max, which the OpenAI-compatible list does not",
    () => {
      assert.ok(PROVIDER_EFFORTS.deepseek.includes("max"));
      assert.ok(
        !PROVIDER_EFFORTS.openaiCompatible.includes("max"),
        "the OpenAI-compatible list is what local servers read, and `max` is DeepSeek's",
      );
    },
  ],
];

let failed = 0;
for (const [name, fn] of tests) {
  try {
    fn();
    console.log(`  ok  ${name}`);
  } catch (e) {
    failed += 1;
    console.log(`FAIL  ${name}`);
    console.log(`      ${e instanceof Error ? e.message : String(e)}`);
  }
}
console.log(`\n${tests.length - failed}/${tests.length} passed`);
if (failed) process.exit(1);
