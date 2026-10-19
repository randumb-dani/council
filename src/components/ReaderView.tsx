import { useDeferredValue, useEffect, useMemo, useState } from "react";
import { ArrowCounterClockwise } from "@phosphor-icons/react/dist/csr/ArrowCounterClockwise";
import { ArrowLeft } from "@phosphor-icons/react/dist/csr/ArrowLeft";
import { DownloadSimple } from "@phosphor-icons/react/dist/csr/DownloadSimple";
import { BoardPanel, Transcript, TranscriptSearch } from "./Panels";
import { RecordPanel, endLabel, endTitle } from "./RecordPanel";
import { TabPanel, TabStrip } from "./Tabs";
import { loadDebate, save, slug, type SavedDebate } from "../debates";
import { seatGlyphs, seatSlots } from "../seats";
import { draftFrom, saveResumeDraft } from "../resume";
import type { CouncilState } from "../useCouncil";

/**
 * A debate, read.
 *
 * Everything here is the same components the live chamber uses — the board,
 * the transcript, the record — handed a saved state instead of a running one.
 * That is the whole reason for the router: one set of components, so a debate
 * cannot look one way while it is running and another way once it is filed.
 *
 * No composer, no Skip, no extension banner. There is nothing here to answer.
 */
export function ReaderView({
  id,
  onBack,
  onResume,
}: {
  id: string;
  onBack: () => void;
  /** Into the setup panel, carrying the roster and the record so far. */
  onResume: () => void;
}) {
  const [debate, setDebate] = useState<SavedDebate | null | undefined>(undefined);
  const [tab, setTab] = useState<"board" | "record">("record");
  const [query, setQuery] = useState("");
  const [exportError, setExportError] = useState<string | null>(null);

  useEffect(() => {
    setDebate(loadDebate(id) ?? null);
  }, [id]);

  const state: CouncilState | null = debate?.state ?? null;
  const colorOf = useMemo(
    () => (seat: string) => state?.delegates.find((d) => d.id === seat)?.color ?? "#6c8b84",
    [state],
  );
  const labelOf = useMemo(
    () => (seat: string) => state?.delegates.find((d) => d.id === seat)?.label ?? seat,
    [state],
  );
  // Same marks and ramp positions the live chamber uses, derived from the saved
  // roster — a read debate has to look like the debate did.
  const { glyphOf, slotOf } = useMemo(() => {
    const glyphs = seatGlyphs((state?.delegates ?? []).map((d) => ({ id: d.id, role: d.role })));
    const slots = seatSlots(state?.delegates ?? []);
    return {
      glyphOf: (seat: string) => glyphs[seat] ?? (seat.charAt(0).toUpperCase() || "?"),
      slotOf: (seat: string) => slots[seat] ?? 1,
    };
  }, [state]);
  // Deferred so keystrokes stay responsive: the input keeps `query` while the
  // expensive scan in `Transcript` sees the lower-priority value. Matching
  // semantics unchanged.
  const deferredQuery = useDeferredValue(query);
  // Memoized: debate is static while typing; these recomputed per keystroke.
  // Declared before the early returns with null guards, so hook order is stable.
  const transcriptInitialCollapsed = useMemo(() => {
    const st = debate?.state;
    if (!debate || !st) return undefined;
    if (debate.summary.unfinished || st.speeches.length === 0) return undefined;
    const max = Math.max(...st.speeches.map((x) => x.round));
    return [...new Set(st.speeches.map((x) => x.round))].filter((r) => r !== max);
  }, [debate]);
  // Memoized: same per-keystroke recompute, this time over the board.
  const boardInitialCollapsed = useMemo(() => {
    const st = debate?.state;
    if (!debate || !st) return undefined;
    if (debate.summary.unfinished) return undefined;
    return st.points.map((p) => p.id);
  }, [debate]);

  if (debate === undefined) {
    return <div className="p-10 text-[12.5px] text-ink-3">opening…</div>;
  }
  if (debate === null || !state) {
    return (
      <div className="flex h-full flex-col">
        <Bar onBack={onBack} title="no such debate" />
        <div className="p-10 text-[12.5px] text-ink-3">
          That debate is not in the library. It may have been deleted since you
          opened the link, or opened in a different window.
          <div className="mt-3">
            <button type="button" className="btn" onClick={onBack}>
              back to the library
            </button>
          </div>
        </div>
      </div>
    );
  }

  const s = debate.summary;
  const summarizer = state.delegates.find((d) => d.role === "summarizer")?.id ?? null;
  // What resuming would carry. Null unless the debate was cut short *and* has
  // speeches to hand over, so the button is never a dead end.
  const resume = s.unfinished ? draftFrom(debate.id, state, s) : null;

  return (
    <div className="flex h-full flex-col">
      <Bar
        onBack={onBack}
        title={s.motion || "untitled motion"}
        sub={
          s.unfinished
            ? `unfinished — ${s.rounds} round${s.rounds === 1 ? "" : "s"} spoken`
            : `${endTitle(state) || endLabel(state)}`
        }
        right={
          <div className="flex gap-1">
            {/* Only on an unfinished debate, and only when there is something to
                continue — an empty card gets no button to click. */}
            {resume && (
              <button
                type="button"
                className="btn btn-primary !px-2 text-[11px]"
                title="Argue this motion again, with the record so far handed to every delegate. The board starts empty — you pick the models."
                onClick={() => {
                  // A full store loses the draft: navigating anyway would land on
                  // the setup with nothing resumed and no explanation.
                  if (!saveResumeDraft(resume)) {
                    setExportError(
                      "The saved setup store is full, so the resume could not be kept. Free some space and try again — the debate itself is still in the library.",
                    );
                    return;
                  }
                  onResume();
                }}
              >
                <ArrowCounterClockwise size={12} /> resume
              </button>
            )}
            <button
              type="button"
              className="btn btn-ghost !px-2 text-[11px]"
              title="download the readable record"
              onClick={() => {
                void save(
                  `${slug(s.motion)}-${debate.id}.md`,
                  debate,
                  "md",
                  "text/markdown",
                ).then((why) => setExportError(why));
              }}
            >
              <DownloadSimple size={12} /> md
            </button>
            <button
              type="button"
              className="btn btn-ghost !px-2 text-[11px]"
              title="download everything, as JSON"
              onClick={() => {
                void save(
                  `${slug(s.motion)}-${debate.id}.json`,
                  debate,
                  "json",
                  "application/json",
                ).then((why) => setExportError(why));
              }}
            >
              json
            </button>
          </div>
        }
      />
      {exportError && (
        <p className="shrink-0 border-b border-bad/60 bg-bad-soft px-4 py-1.5 text-[12px] text-bad" role="alert">
          {exportError}
        </p>
      )}
      {resume && (
        <p className="shrink-0 border-b border-line bg-primary-soft px-4 py-1.5 text-[11.5px] leading-relaxed text-ink-2">
          This debate never closed. <strong className="font-semibold text-ink">Resume</strong>{" "}
          opens the setup with the same motion and the same delegates, and hands
          everyone the record above — but the board starts empty, because the old
          one was only ever in memory. Pick the models again: a server not running
          the old one now would fail on the first turn.
        </p>
      )}

      <main className="chamber min-h-0 flex-1" data-left="0" data-right="1">
        <section className="flex min-h-0 min-w-0 flex-col">
          <div className="shrink-0 border-b border-line bg-surface px-5 py-2">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11.5px] text-ink-3">
              <span className="tabular-nums">
                {s.carried} carried · {s.fell} fell · {s.stalemate} tied · {s.points} points
              </span>
              <span>{s.speeches} speeches</span>
              <span className="text-ink-3">
                {s.seats.map((x) => x.label).join(" · ")}
              </span>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-5 py-1.5">
            <TranscriptSearch value={query} onChange={setQuery} />
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto stable-gutter px-5 py-4">
            <Transcript
              key={debate.id}
              speeches={state.speeches}
              live={null}
              colorOf={colorOf}
              labelOf={labelOf}
              glyphOf={glyphOf}
              slotOf={slotOf}
              polls={state.polls}
              summarizerId={summarizer}
              reads={state.reads}
              query={deferredQuery}
              // Land on the end of a finished debate, not its opening.
              initialCollapsed={transcriptInitialCollapsed}
            />
          </div>
        </section>

        <aside className="chamber-right flex min-h-0 flex-col overflow-hidden border-l border-line bg-surface">
          <TabStrip
            label="board and record"
            value={tab}
            onChange={setTab}
            tabs={[
              { id: "board", label: "Board", badge: state.points.length },
              { id: "record", label: "Record" },
            ]}
          />
          <TabPanel id={tab}>
            {tab === "board" ? (
              <BoardPanel
                points={state.points}
                delegates={state.delegates}
                tallyOf={(pid) => state.tallies[pid]}
                labelOf={labelOf}
                slotOf={slotOf}
                voteLog={state.voteLog}
                /* The reader is reference material: every point's exchanges
                    start folded, because a 40-point board otherwise opens every
                    rebuttal in full. Settled points fold anyway; this takes the
                    open and contested ones too. */
                initialCollapsed={boardInitialCollapsed}
              />
            ) : (
              <RecordPanel state={state} />
            )}
          </TabPanel>
        </aside>
      </main>
    </div>
  );
}

function Bar({
  onBack,
  title,
  sub,
  right,
}: {
  onBack: () => void;
  title: string;
  sub?: string;
  right?: React.ReactNode;
}) {
  // The `ref` this used to declare was attached and never read: nothing in the
  // bar measures or focuses anything. Removed rather than left as a hook that
  // looks load-bearing.
  return (
    <header className="flex min-h-[52px] shrink-0 items-center gap-3 border-b border-line bg-surface px-4 py-2">
      <button type="button" className="btn btn-ghost" onClick={onBack} title="back to the library">
        <ArrowLeft size={14} /> library
      </button>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[12.5px] font-semibold tracking-tight text-ink" title={title}>
          {title}
        </div>
        {sub && (
          <div className="truncate text-[11px] text-ink-3" title={sub}>
            {sub}
          </div>
        )}
      </div>
      {right}
    </header>
  );
}