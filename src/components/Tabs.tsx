import { useRef } from "react";

/**
 * The Board/Record switch, once.
 *
 * This existed twice with two different sets of semantics: the live chamber's
 * inline copy carried `role="tablist"` + `role="tab"` + `aria-selected` and no
 * keyboard support, and the reader's `Tab` component carried `aria-pressed` and
 * no roles at all. Same class strings, same transition, drifted. Whichever one
 * got fixed, the other stayed broken.
 *
 * The ARIA tab pattern is now implemented properly rather than partly:
 *
 *  - `role="tablist"` with an accessible name, and `role="tab"` / `role="tabpanel"`
 *    on the children and the pane they swap.
 *  - Arrow keys move and select, Home and End jump to the ends, which is what
 *    `role="tab"` promises. Tab moves *out* of the strip, because in this
 *    pattern Tab is not navigation within the tabs.
 *  - Roving `tabIndex`: only the selected tab is in the tab order, so a keyboard
 *    user reaches the strip once and then uses arrows. This is the part the old
 *    half-implementation got backwards — two tabs, both tabbable, no arrows.
 *
 * `tabIndex` on the unselected tabs being -1 is what makes that work, and it is
 * also why the arrow handling below has to move *selection* as well as focus:
 * with roving focus and no automatic activation, a focused-but-unselected tab
 * would leave the panel showing something the focus ring is not on.
 */

export interface TabSpec<T extends string = string> {
  id: T;
  label: string;
  /** Optional count badge, e.g. the number of points on the board. */
  badge?: number;
}

export function TabStrip<T extends string>({
  tabs,
  value,
  onChange,
  label,
}: {
  tabs: readonly TabSpec<T>[];
  value: T;
  /** Typed to the tab ids, so a caller cannot pass a string the panel will not
      render — the mismatch the two hand-rolled copies of this strip kept making. */
  onChange: (id: T) => void;
  /** Accessible name for the group: what these tabs switch between. */
  label: string;
}) {
  const stripRef = useRef<HTMLDivElement>(null);
  const index = Math.max(
    0,
    tabs.findIndex((t) => t.id === value),
  );

  const focusTab = (next: number) => {
    const clamped = (next + tabs.length) % tabs.length;
    const buttons = stripRef.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    buttons?.[clamped]?.focus();
    onChange(tabs[clamped]!.id);
  };

  return (
    <div
      ref={stripRef}
      className="sticky top-0 z-2 flex shrink-0 border-b border-line bg-surface"
      role="tablist"
      aria-label={label}
      onKeyDown={(e) => {
        switch (e.key) {
          case "ArrowRight":
          case "ArrowDown":
            e.preventDefault();
            focusTab(index + 1);
            break;
          case "ArrowLeft":
          case "ArrowUp":
            e.preventDefault();
            focusTab(index - 1);
            break;
          case "Home":
            e.preventDefault();
            focusTab(0);
            break;
          case "End":
            e.preventDefault();
            focusTab(tabs.length - 1);
            break;
          default:
            break;
        }
      }}
    >
      {tabs.map((t) => {
        const on = t.id === value;
        return (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-selected={on}
            aria-controls={`panel-${t.id}`}
            // Roving: the strip is one tab stop, not one per tab.
            tabIndex={on ? 0 : -1}
            onClick={() => onChange(t.id)}
            className={`flex-1 border-b-2 px-3 py-3 text-[11px] font-semibold tracking-[0.1em] uppercase transition-colors ${
              on ? "border-primary text-ink" : "border-transparent text-ink-3 hover:text-ink-2"
            }`}
          >
            {t.label}
            {t.badge != null && t.badge > 0 && (
              <span className="ml-1.5 rounded-pill bg-surface-3 px-1.5 text-[10px] tracking-normal text-ink-2 tabular-nums">
                {t.badge}
              </span>
            )}
            <span className="sr-only">{on ? " (selected)" : ""}</span>
          </button>
        );
      })}
    </div>
  );
}

/** The pane a tab swaps, with the wiring its `aria-controls` promises. */
export function TabPanel({
  id,
  children,
}: {
  id: string;
  children: React.ReactNode;
}) {
  return (
    <div
      role="tabpanel"
      id={`panel-${id}`}
      aria-labelledby={`tab-${id}`}
      className="min-h-0 flex-1 overflow-y-auto stable-gutter"
    >
      {children}
    </div>
  );
}