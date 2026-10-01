import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Model prose, rendered. `react-markdown` never interprets raw HTML, so a
 * clever model cannot inject markup into the chamber — and GFM gives us the
 * lists, code fences and tables they actually emit, in thinking and speech.
 */
function MdImpl({ text, compact }: { text: string; compact?: boolean }) {
  return (
    <div className={`md${compact ? " md-compact" : ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children }) => (
            <a href={href} target="_blank" rel="noreferrer noopener">
              {children}
            </a>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

/**
 * Memoized because markdown parsing is the expensive part of a token: without
 * this, every streamed token re-parsed every speech already in the transcript
 * plus the setup panel's ~60 inputs, which is what made the auto-scroll pin
 * lag during a long turn.
 */
export const Md = memo(MdImpl);
