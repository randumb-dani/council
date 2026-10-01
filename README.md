# Council

A council chamber for many models at once.

Point a motion at two or more models served by any OpenAI-compatible endpoint
(Unsloth Studio, llama.cpp, vLLM, LM Studio, Ollama, OpenRouter, OpenAI) and
let them argue it out. Every model raises claims onto a shared **board**, and
every model rates every claim **win / lose / stalemate**. Concede what you have
lost, poll the room when argument stalls, and when the debate closes you get a
verdict that was computed from the board rather than summarised from vibes.

Tauri v2 + React + TypeScript. Flat surfaces, one accent colour, and a council
rail that tells you who has said what — no 3D, no renders. Linux and Windows.

---

## The board is the point

Prose is not evidence of anything. A model saying "my point is right" is worth
nothing, so the engine never takes a model's word for it. The only things that
change the outcome are tool calls, and every one of them is validated and
applied by Rust.

**The core mechanic** is `vote`: every delegate, on every point, casts one of
three verdicts.

| | meaning |
|---|---|
| `win` | the point survives scrutiny — it stands |
| `lose` | the point is refuted — it falls |
| `stalemate` | the record genuinely cannot decide it |

A point's score is `wins − losses`. A point's proponent implicitly rates their
own claim `win` until they vote otherwise, concede it, or withdraw it — which is
what an advocate actually does, and without it a three-seat council could never
carry anything. A split that cancels out is a `stalemate`; a point is only
`carried` or `fell` when the board has actually decided it.

### The toolbelt

| tool | what it does |
|---|---|
| `make_point` | put a new falsifiable claim on the board, optionally offered as defeating existing points |
| `vote` | rate a point win / lose / stalemate — you may change your vote and are asked to say why |
| `concede` | give a point up; conceding *your own* point retracts it |
| `withdraw` | retract a point you raised and no longer hold |
| `poll` | put a fixed set of options to every other delegate; they are polled immediately and the result is recorded |
| `steelman` | restate another delegate's point in its strongest form |
| `rebut` | attack a specific point with a counter-argument |
| `cite` | put named evidence on the record, tagged supports / undermines / neutral |
| `press` | demand a direct answer from a named delegate; they must answer on their next turn |
| `adjourn` | argue the record is exhausted and stop |
| `propose_extension` | Summarizer only, between rounds: ask the operator for more rounds |

A poll is not a point and does not touch the board. A tie is reported as
inconclusive rather than being broken for you.

**One claim, one point.** A model that re-raises a claim — next round, or twice
in one reply — is refused rather than admitted a second time, and the refusal
names the point already on the board so it can vote on it or press its author
instead. Claims are compared with case, spacing and terminal punctuation
normalised away, since none of those carry meaning here. A *rephrasing* is not
caught: telling a reworded claim from a sharpened one needs judgement, and the
engine refuses to guess — a false merge could swallow the refutation of a point
and quietly break the tally.

Not every server does native tool calls, so actions are also scraped from the
text: a fenced block, or a bare object on a line of its own. Scraping pulls
every well-formed object out of what the model wrote rather than requiring the
whole block to parse, because models pretty-print their JSON, add a closing
remark after it, and put two actions in one block — each of which used to drop
the action silently. A point id is only accepted when the text names exactly one
number, so `"P1 beat P2"` is refused rather than guessed at as point 12.

---

## Running it

### Prerequisites

- Node 18+ and Rust (stable)
- Linux only: the Tauri system dependencies

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk4.1-devel openssl-devel libappindicator-gtk3-devel \
  librsvg2-devel

# Arch
sudo pacman -S webkit2gtk-4.1 base-devel curl wget file openssl \
  appmenu-gtk-module librsvg
```

`libwebkit2gtk-4.1-dev` (note the `.1`) is the one that trips people up; the
older `libwebkit2gtk-4.0-dev` is not enough for Tauri 2.

### Develop

```bash
npm install
npm run tauri dev
```

### Build an installer

```bash
npm run tauri build
```

### Work on the interface alone

```bash
npm run dev
```

Opened in a plain browser (not inside Tauri) the app runs a scripted stand-in
debate so the interface is usable without an endpoint. It says so in a banner.

---

## Connecting to Unsloth Studio

Unsloth serves the OpenAI-compatible API on the same port as its UI, and prints
the endpoint URL and an API key when it starts. It binds `127.0.0.1` only.

```bash
unsloth studio -p 8888        # or: unsloth run --model <model>
```

In the **Endpoints** panel:

- **Base URL** — `http://localhost:8888` is enough. The `/v1` is added for you
  when the base has no path, and if the server answers 404 the other shape is
  tried automatically. `http://localhost:8888/v1` also works, as does pasting
  the full `.../v1/chat/completions` URL.
- **API key** — required. Unsloth issues an `sk-unsloth-…` key under
  avatar -> Settings -> API and only shows it once. Without it you get a 401.
- **test connection** — calls `/v1/models` and shows the real reason for any
  failure rather than a generic one.

Three Unsloth-specific gotchas, all handled but worth knowing:

- `localhost` is forced to `127.0.0.1`. Unsloth binds IPv4 only, and a resolver
  that answers `localhost` with `::1` first produces a connection refused from a
  client that would otherwise work.
- If you launched with `--secure`, the request goes through a Cloudflare quick
  tunnel, and **server-sent events do not survive it**. Untick **stream** on that
  endpoint; the client also falls back to a whole-response request by itself.
- `GET /v1/models` only lists models that are **currently loaded**, so an empty
  list means nothing is loaded rather than a broken connection.

Note that `npm run dev` in a plain browser never contacts an endpoint — it runs
the scripted stand-in. Use `npm run tauri dev` for the real thing.

---

## Layout

```
crates/council-core/     the engine — no Tauri, no UI, fully testable
  types.rs               the board: points, votes, tallies, polls, verdicts
  tools.rs               tool schemas + a deliberately lenient argument parser
  client.rs              OpenAI-compatible streaming client (SSE, tool deltas)
  prompt.rs              system prompt and the per-turn board snapshot
  engine.rs              the loop: turns, rounds, polls, closing, verdict
  web.rs                 the reference shelf (Wikipedia/Wiktionary), allow-listed
  decide.rs              decision-model ballots (Jev-style classification)
  events.rs              the event stream the UI renders from, and nothing else
src-tauri/               a thin bridge — start, stop, interject, forward events
src/                     React UI
  useCouncil.ts          the reducer that accumulates the event stream
  debates.ts             the library: storing, summaries, Markdown/JSON export
  bridge.ts              Tauri invoke + event subscription (scripted stand-in outside Tauri)
  components/            council rail, setup, transcript, board, record, reader
```

The Rust engine is authoritative. The webview never scores a point, never
resolves a vote, and never decides a status — it renders the engine's answers.
That rule is what keeps the interface from quietly disagreeing with the record.

### Testing

```bash
npm run test:core     # the board, the tool parser, the whole engine, scripted end to end
npm run check         # tsc --noEmit
```

The engine tests drive a scripted backend through the real state machine, so
scoring, polls, conceding, pressuring, adjournment and the event-stream JSON
shape are all covered without a network. One of them asserts that no
snake_case ever reaches the webview, because that is the failure that would
otherwise show up as a silently blank panel.

---

## Notes on the implementation

**Tool calling.** Native tool calls are used when the endpoint supports them.
Toggle *native tool calls* off per endpoint and the engine falls back to asking
for a fenced JSON block and scraping it — the same parser runs either way, so
small models and servers without a tool-calling template still work.

**Context.** Each model keeps its own conversation, sees the whole board
regenerated fresh every turn (so it can never work from a stale board), and is
shown only the record it has missed. Its own history is trimmed to the opening
plus the last few turns. A point's text is clipped before it goes into a prompt.

**Cancellation** is cooperative and checked between turns and before every
request. `stop` ends the debate and still renders a verdict for whatever is on
the board.

**Slow turns.** While a request is in flight the engine emits a heartbeat every
few seconds, so the UI can show elapsed time and tokens rather than a dead
cursor. If the count stays at zero, the model is still loading or the request is
queued — with Unsloth's *switch model by request* on and a different model per
seat, expect minutes of silence while a large model loads. **Skip** abandons the
current speaker's turn and moves on without killing the debate.

**Scoring lives in one place.** `Point::tally` in Rust produces the score, the
status, and the effective rating of every seat; the `Tally` is shipped with each
board update and the UI just draws it.

**Thinking is first-class.** Servers that expose it (`reasoning_content` and
friends) get it streamed into a collapsible thought block on the live turn and
kept on the finished speech — never fed back into anyone's history, and tool
calls hiding in the thinking still count.

**A model that just stops gets named.** An empty turn is logged as one instead
of leaving a hole, a `length` stop names the server cutting a reply short
(speaking turns are uncapped), and a content-filter stop says so outright.

**Decisions without prose.** Poll ballots are classification, so the Decisions
section can route them to a decision model (TypeSafe's Jev and its kin: typed
question in, choice plus calibrated confidence out, no tokens generated) instead
of burning a full LLM turn per voter. A failed call falls back to the seat's own
model and says so; votes stay with the seats, because they need reasons.

**No burying deadlocks.** Adjourning over a stalemate is refused until the point
has been through a deadlock review: any seat calls `review`, every other seat
rules it unsolvable, too dense for this council, or decidable — and may change
its vote on the point in the same breath, which the tally applies immediately.
The ruling rides on the point, into the board card and the record. A fresh
review supersedes the old one: sides may change their minds twice.

**Closing takes consensus.** Adjourning is a motion, not a gavel: the mover's
turn pauses while every other seat confirms or denies, the mover counting as
yes and a strict majority of seats required. A failed motion is announced and
cannot be moved again until the next round; unreviewed stalemates still block
the motion outright.

**Rounds are narrated by the Summarizer.** When a round fully ends, the
intermission belongs to the Summarizer seat: it states what moved, what stands,
what is still open. The summary goes on the record, so later rounds argue from
a shared account. Polls, meanwhile, always interrupt: a `poll` call pauses its
turn, every other seat answers immediately, and only then does the opener's
speech finish. Open polls render inline under the live turn; the top bar
carries round segments so the debate's pace is visible at a glance.

**The Summarizer can ask for more rounds.** Between rounds its only tool is
`propose_extension { rounds, reason }`; once the council has closed it trades
that for `conclude`. The two never appear in the same belt.

**It can also ask *after* seeing the deadlock.** On a settled board the closing
turn offers only `conclude` — there is nothing more rounds could fix. But when
the board deadlocked, `propose_extension` is offered again at that final
moment: the summarizer can see that a point nobody engaged, or a side never
heard, might break with another pass, and say so. Approve and the debate
resumes at the next round and concludes again; decline and the Answer section
reports no agreement with the lean, as it would have anyway. Still one request
per debate, so a summarizer that keeps finding new reasons cannot reopen the
council until the budget is gone. Approved rounds are all actually spoken —
`ignore_settled` carries the debate past the tally that stopped it, so a grant
of two rounds delivers two rounds. Calling it suspends the debate and puts
the request to you, with an editable count. Approve and the debate grows;
decline and the summary is laid out as usual. It is bounded four ways: one
proposal per intermission, an extension budget you set in Procedure (granted
rounds spend from it, and the header shows what is left), no proposal at all
once the budget is gone — the prompt stops advertising the tool — and a
five-minute deadline, after which an unanswered request is declined rather than
suspending the debate indefinitely. Only the Summarizer can propose; a speaker
trying it mid-turn is refused.

**A tool call is never shown as speech.** When a model writes its call inline
instead of using native tool calling, the engine acts on it *and* takes it back
out of the transcript — what the seat said is the sentence around the JSON, not
the JSON. A quoted example of the protocol is left alone: it is prose, so it
stays prose.

**No answer is ever capped.** Every request the engine makes — speaking turns,
ballots, deadlock rulings, closing addresses, summaries, the end result — is
sent uncapped. A tight budget looks harmless and is not: a reasoning model
spends most of it on thinking, and what survives is cut mid-sentence. A
truncated *speech* at least looks short; a truncated *ballot* stops parsing
entirely and the seat is silently counted as having abstained, which moves the
majority on a motion it never got to vote on. If the server reports a cut
anyway, the UI names it.

**Nothing the summarizer writes is truncated silently.** Its intermission and
its end result are uncapped, like a speaking turn — a reasoning model spends
most of a small token budget on thinking, and a result cut at 600 tokens landed
in the sidebar as a half-finished sentence. If the server cuts one anyway, the
UI says so rather than presenting it as a conclusion.

**The round always gets laid out.** With *stop early once every point is
decided* on, the council still finishes the round in progress before stopping:
every seat is heard and the summarizer still summarises. Cutting the round
short at the moment it settled left the transcript with no account of what had
happened. An *adjournment*, by contrast, ends the debate at once.

---

## The essential seats

A council is five fixed seats plus any number of seats you add yourself.

| seat | what it does |
|---|---|
| **Advocate** (proponent) | makes the case |
| **Critic** (opponent) | steelmans the other side, then attacks it |
| **Arbiter** (judge) | rules: one vote a turn, plus the power to settle a point outright |
| **Rationale** (rationale) | finds logical fallacies; argues and votes, sitting after the Arbiter |
| **Summarizer** | lays out each round and the end result; never argues |

Their labels are fixed and they cannot be removed — everything else about a
seat (model, endpoint, role line, voice, temperature) configures like any
other. Added seats are **custom**: they argue, vote, poll and take turns in
normal seat order. The engine refuses to convene a setup that is missing one
of the four convening essentials or seats two of one; the Rationale is
permanent furniture but optional, so at most one may sit and a council without
one still convenes. Saved setups predate it, so one is appended (or your
custom seat named Rationale promoted in place) the next time the setup loads.

The Summarizer never argues. It takes no round turn, casts no poll, review or
adjournment ballot, and cannot move to adjourn — its instrument is the record.
Once the debate closes it writes the **end result**, which lands at the top of
the Record tab.

### The answer

At the foot of the Record tab, above the end result, is the **Answer**: the
motion answered directly, in a sentence or two. The Summarizer gets one more
instrument for it — `conclude { agreed, answer, lean }` — and is told to be terse:
this is the bottom of the page, not a closing speech.

**The engine decides whether there is an answer.** `agreed` is the board's
verdict, not the seat's claim: if any point is still open or deadlocked, the
council did not agree no matter what the Summarizer says, and a claim of
consensus is dropped and reported rather than shown. When it did not agree, the
section says so plainly — *the council could not agree on an answer* — and then
shows the **lean**: which way the evidence pointed anyway, with the tally behind
it. "No agreement" and "nothing happened" are different facts, and the section
keeps them apart. A setup saved before this change is repaired on load: the three
old seats take the arguing roles and a Summarizer is appended with the first
seat's model, so old debates convene without repair.

Polls, reviews and adjournment votes are counted across the seats that may
vote — the Summarizer abstains by construction, and a strict majority is taken
of the voting seats.

## The reference shelf

A debating seat sometimes needs to know what a word means, or what a position
actually claims, and it cannot get either from its own weights. Switch on
**“let the advocate and critic look terms up”** in Setup and two instruments
appear:

| tool | what it does |
|---|---|
| `web_search { query }` | full-text search across Wikipedia and Wiktionary, plus a one-line definition from DuckDuckGo when there is one |
| `web_fetch { title, source? }` | the plain text of one article; `source: auto` tries Wikipedia then the dictionary, so a term and a topic are the same call |

### It is not a search engine

Real web search needs a paid API key — Brave, Serper, Google. Every keyless
scrape of a search engine now answers with a bot challenge or a 403: DuckDuckGo's
HTML endpoint returns a challenge page, Mojeek returns 403. So there is no honest
way to offer "search the web" here without asking you for a key, and the shelf is
instead a real, free, documented search over an encyclopaedic corpus. It reads
Wikipedia and Wiktionary. It does not read anything else.

### Reading happens inside the turn

A fetch is the first tool here that answers the *model* rather than the board,
and answering the model takes a round trip the turn had not budgeted for. So a
read suspends the turn: the page comes back inside `<untrusted_data>`, the seat
is asked to argue from it, and it replies again — with its speech and its board
actions from that second reply.

That has two consequences worth knowing:

- **A seat that looks something up acts on it in the same turn.** A fetch on its
  own is treated as an action, so a seat that researches and then speaks is not
  nagged about inaction.
- **Actions said before the read are held, not applied.** A seat that stopped to
  look something up did so *because* it wanted to know before committing — and
  reading then voting is the normal case, not a special one. If the reading turn
  produces nothing at all, the earlier calls are used anyway, so a `make_point` is
  never lost to a lookup.

Limits per turn, both configurable: **3 pages** and **6,000 characters** each.
Reads beyond the budget are refused by name rather than dropped quietly, and a
seat is told what it has left so it can fetch the one article a search turned up
without wandering off through a page per source.

A seat may read at most twice within a turn — enough for search-then-fetch-then-
argue, which is the sequence that works — after which it must act on what it
has.

### What a seat may read

- **Only the Advocate, the Critic and the Rationale.** The Arbiter is excluded deliberately: a
  judge who can look things up stops ruling on the record and starts ruling on
  the web, and the verdict is supposed to be made from the record. The Summarizer
  summarizes and does not argue, so a page in its hands is a page in the answer.
- **Off by default.** This is the only capability in the app that reaches off the
  machine, and a debate should not acquire one without being asked.
- **Only Wikipedia, Wiktionary, Wikidata, Wikibooks, Wikisource, Wikiquote and
  DuckDuckGo's instant-answer API**, over https. An allow-list, not a deny-list.
  Userinfo (`https://en.wikipedia.org@192.168.0.41/`), explicit ports, and
  redirects are all refused — a redirect is reported, never followed, because an
  allowed host answering 302 to your LAN reaches the LAN just the same.

### Retrieved text is data, never instruction

Wikipedia is user-editable, so a page can contain text aimed at the model reading
it. Retrieved text therefore arrives inside `<untrusted_data source="...">`, cut
at the first thing that looks like the closing tag — the fence is defeated by a
page that contains the fence — and the prompt tells the seat plainly that what is
inside is reference material and never something to obey.

Every page a seat reads appears in the transcript as an expandable row with its
title, a link, and the full text, so you can open the same page it did. A seat
citing a source you cannot see is no more trustworthy than one inventing it, and
the invented one at least stays consistent.

## Arguing over images

Attach up to six images to the motion and every seat sees them with its
opening message, named Image 1..N. They ride along as `image_url` parts on a
standard OpenAI-compatible request, so anything that serves a vision-capable
model (llama.cpp with a multimodal model, vLLM, OpenRouter, OpenAI) works
unchanged — but a text-only model will fail the turn instead, so point image
seats at vision models.

Two practical limits: picks are downscaled to 1568px JPEG on the way in, so a
phone photo bills hundreds of image tokens rather than thousands, and image
*bytes* never reach the library or the exports — the record keeps the file
names, not the files.

## The library

A debate is four models and however many rounds you afforded it. Until this
existed it lived only in the window: closing the app lost it, and there was no
way to take it anywhere else.

There are three places to be, reachable by the address bar and by the back
button:

| | |
|---|---|
| `#/` | **the library** — every debate kept, listed. Nothing is auto-opened |
| `#/new` | the chamber: setup, then Convene |
| `#/d/<id>` | **the reader** — one debate, read-only, no composer |

Every debate is written into the library as it runs, debounced, and
immediately once it reaches a verdict. The last card is therefore never a
half-finished window you closed by accident.

**Taking one out** gives two files: a readable **Markdown** record (motion, how
it ended, the Answer, the scored board as a table, every speech grouped by
round, the polls, and the pages anyone read with their links) and the **raw
JSON** for anything you want to analyse or re-import.

### What is and is not kept

Saved debates are never stored running, mid-turn, or waiting on you — reopening
one must not look like a debate that is somehow still going. The last six
notices travel with the record, because that is usually the debate that went
wrong and you will want to read why later.

**No API keys are ever written.** The config holds them; only the state is
persisted, and only model *names* are recorded from it.

`localStorage` is about 5MB and a long debate's reasoning is the bulk of it, so
the library keeps the newest **20**, sheds the oldest when it must, and — only
as a last resort — strips *thinking* from the oldest entries. Never the
speeches: a record without the argument is not a record.

## Reading a long debate

Six rounds is twenty-four speeches. What the reader adds:

- **Round dividers**, sticky, with a count, and **click to fold a round away**.
  Reopening a finished debate lands on its last round, not its opening.
- **Search** across speech text and action lines, reporting which rounds match.
- **Point lineage** — which claim defeats which. This was recorded by the engine
  the whole time and never drawn.
- **Changes of mind.** Every seat that revised a vote is listed under the point,
  with the round it happened in and why. A seat changing its view mid-debate is
  the most interesting thing on the board, and it used to look identical to a
  seat that never had one.
- **Every remark**, not only the last, so a three-round exchange reads as three
  rounds rather than as its conclusion.

## Counting what it costs

Per endpoint, **count tokens** is on by default. A streamed response only
reports usage when `stream_options.include_usage` is set, and a server that does
not know the field rejects the whole request rather than ignoring it — so when
that happens the engine asks once more without it, remembers the refusal for
the session, and the turn goes ahead either way. One wasted round trip the
first time, none after.

With it on, each seat's rail row shows the server's own prompt and completion
counts and time on the clock — never an estimate. The mid-turn "tok" figure is
stream progress, not a bill; the rail totals are the bill. A seat with no
figure is *unmeasured*, not free: no event is emitted at all when the server
does not answer, so the two are never confused.

## Running the checks

```sh
npm test           # core (rust) + export formats (node) + tsc
npm run test:core  # rust only, TLS off — needs no C compiler
npm run test:tls   # rust with TLS, the default — needs a working C compiler
npm run test:web   # the export formats only
```

`test:core` runs with `--no-default-features` because TLS pulls in `ring`, which
wants a C toolchain. That is the only reason for the flag: the engine itself
does everything over plain HTTP, and CI has no compiler. `test:tls` covers the
TLS path when you have one.

`npm run test:web` covers the export formats, which are the only artifacts a
debate leaves by hand: a `|` or a newline in a claim silently corrupting a
Markdown table is exactly the sort of thing a test should catch.

## The Arbiter rules

The Arbiter used to be able to vote on every point in a turn. That looked like
influence and was not: it was one seat among three, two of which had already
spoken, so it could shift a tally but never settle anything.

It now has two instruments:

- **one ordinary vote a turn**, and
- **`rule { point, ruling, reason }`** — the point *carries* or *falls*,
  whatever the votes say. One a turn, offered to that seat and no other, and
  refused without a reason the record could stand behind.

**The tally is left alone.** A ruling that carried a point scoring minus one
shows as exactly that: the Arbiter overrode the count. Rewriting the count to
match would hide the only interesting thing that happened.

So the thing a judge is actually for — saying a point is settled when the
counting does not show it — is now possible, and the board says who did it and
why.

## Asking for more rounds is a vote, not a button

The Summarizer can still ask for extra rounds. What changed is who decides.

The request opens a **modal over a blurred debate**, the seats are asked one at a
time, and the tally builds on screen as they answer. A majority carries it. With
three seats there is no tie, so the request always resolves — the debate no
longer parks waiting for a human to come back.

**The operator can still settle it**, at any point while the room is counting,
and their answer is the answer: three yeses do not overrule a veto. The buttons
are a veto, not the mechanism, and the modal says so.

A seat that fails or says nothing is recorded as having not answered, not as
having voted against — a silent seat is one seat lighter, which is a different
fact and can change the majority.

## What a seat is told

Every seat gets the same shape of prompt, and the shape is built from what that
seat can actually do. The rule the prompt code exists to hold:

> **The prompt describes the belt that seat is carrying, and nothing else.**

It is worth stating plainly because the alternative is worse than verbose. A
model told about an instrument it does not have will reach for it, and every one
of those reaches comes back refused — which teaches it nothing, spends the turn,
and leaves a trace on the record that looks like the model misbehaving.

Concretely:

- **`rule` is described to the Arbiter alone.** Two thirds of the council cannot
  reach it, so two thirds never hear of it.
- **The inline call format is described only to servers that need it.** A server
  with native tool calling is never told to write a fenced JSON block; a server
  without it is given the envelope, the legal shapes, and concrete examples.
  Without this the scraper was waiting for a format no prompt ever mentioned.
- **Reading is described only when the operator has switched it on**, because a
  model told about `web_fetch` will eventually call it.

The prompt opens with what the system *is* — the board, the turn order, what a
turn is, what a poll is, how it ends — before any rules. A seat that has not
been told the shape of the thing guesses at it: it asks the summarizer
questions, or spends a turn explaining what it is about to do.

And where the engine enforces something, the engine says so. A promise the loop
does not keep is worse than no promise, so:

- the Arbiter gets **one vote and one ruling a turn**, both enforced
- a seat whose feed could not carry the whole record is **told how much it missed**
- a seat is **told when the server cut its reply short**, because a lost vote and
  a deliberate abstention are different facts and one of them moves a majority

## Extra seats

Up to six can join the five fixed seats. They argue, vote, and read; they are not
offered `rule`, because that belongs to the Arbiter alone.

Two things are enforced rather than assumed:

- **Seats have distinct names.** `press` addresses a seat by name, so two seats
  sharing one made the first match silently win — a press aimed at the second
  landed on the first, or reported that you cannot press yourself when you meant
  somebody else. `press` also accepts a seat's id, which is what the model
  reaches for when it misremembers a name, and a name that resolves to nothing
  comes back with the roster so it can correct itself in one turn.
- **Every seat is told what it is for.** A seat with no role line falls back to
  its blurb — *"makes the case"*, *"argues the motion"* — so a user-added seat
  knows why it is in the room rather than only that it has the floor.
