import { Scales } from "@phosphor-icons/react/dist/csr/Scales";

/**
 * The wordmark, once.
 *
 * It was byte-identical in the chamber header and the library header, which is
 * how the two drifted into being separate things — and the library's copy was
 * the one that had been edited to fit. A brand rendered twice is edited twice.
 */
export function Wordmark() {
  return (
    <div className="flex items-center gap-2 text-[11px] font-bold tracking-[0.16em] text-ink">
      <span className="flex size-5 items-center justify-center rounded-md bg-primary text-on-primary">
        <Scales size={13} weight="bold" />
      </span>
      COUNCIL
    </div>
  );
}
