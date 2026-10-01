import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, DownloadSimple } from "@phosphor-icons/react";
import { BoardPanel, Transcript, TranscriptSearch } from "./Panels";
import { RecordPanel, endLabel, endTitle } from "./RecordPanel";
import { loadDebate, save, slug, type SavedDebate } from "../debates";
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
}: {
  id: string;
  onBack: () => void;
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

  if (debate === undefined) {
    return <div className="p-10 text-[12.5px] text-ink-3">opening…</div>;
  }
  if (debate === null || !state) {
    return (
      <div className="flex h-full flex-col">
        <Bar onBack={onBack} title="no such debate" />
        <div className="p-10 text-[12.5px] text-ink-3">
          That debate is not in the library. It may have been deleted, or this
          window may be looking at a different store than the one that saved it.
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
              speeches={state.speeches}
              live={null}
              colorOf={colorOf}
              labelOf={labelOf}
              polls={state.polls}
              summarizerId={summarizer}
              reads={state.reads}
              query={query}
              // Land on the end of a finished debate, not its opening.
              initialCollapsed={
                debate.summary.unfinished || state.speeches.length === 0
                  ? undefined
                  : [...new Set(state.speeches.map((x) => x.round))]
                      .filter((r) => r !== Math.max(...state.speeches.map((x) => x.round)))
              }
            />
          </div>
        </section>

        <aside className="chamber-right flex min-h-0 flex-col overflow-hidden border-l border-line bg-surface">
          <div className="sticky top-0 z-2 flex shrink-0 border-b border-line bg-surface">
            <Tab on={tab === "board"} onClick={() => setTab("board")} label="Board">
              {state.points.length > 0 ? state.points.length : null}
            </Tab>
            <Tab on={tab === "record"} onClick={() => setTab("record")} label="Record" />
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto stable-gutter">
            {tab === "board" ? (
              <BoardPanel
                points={state.points}
                delegates={state.delegates}
                tallyOf={(pid) => state.tallies[pid]}
                labelOf={labelOf}
                voteLog={state.voteLog}
              />
            ) : (
              <RecordPanel state={state} />
            )}
          </div>
        </aside>
      </main>
    </div>
  );
}

function Tab({
  on,
  onClick,
  label,
  children,
}: {
  on: boolean;
  onClick: () => void;
  label: string;
  children?: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={on}
      className={`flex-1 border-b-2 px-3 py-3 text-[11px] font-semibold tracking-[0.1em] uppercase transition-colors ${
        on ? "border-primary text-ink" : "border-transparent text-ink-3 hover:text-ink-2"
      }`}
    >
      {label}
      {typeof children === "number" && children > 0 && (
        <span className="ml-1.5 rounded-pill bg-surface-3 px-1.5 text-[10px] tracking-normal text-ink-2 tabular-nums">
          {children}
        </span>
      )}
    </button>
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
  const ref = useRef<HTMLDivElement | null>(null);
  return (
    <header className="flex min-h-[52px] shrink-0 items-center gap-3 border-b border-line bg-surface px-4 py-2">
      <button type="button" className="btn btn-ghost" onClick={onBack} title="back to the library">
        <ArrowLeft size={14} /> library
      </button>
      <div ref={ref} className="min-w-0 flex-1">
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