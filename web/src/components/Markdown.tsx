import ReactMarkdown from "react-markdown";
import { cx } from "./ui";

/** Markdown for memory notes. react-markdown never renders raw HTML, so note content stays inert. */
export function MarkdownBlock({ children, className }: { children: string; className?: string }) {
  return (
    <div className={cx("md break-words text-[13.5px] leading-relaxed [overflow-wrap:anywhere]", className)}>
      <ReactMarkdown>{children}</ReactMarkdown>
    </div>
  );
}

/** A one-line title where `backtick` spans render as code. */
export function InlineCode({ text }: { text: string }) {
  return (
    <>
      {text.split(/(`[^`]+`)/).map((part, i) =>
        part.length > 2 && part.startsWith("`") && part.endsWith("`") ? (
          <code key={i} className="rounded bg-panel-2 px-1 font-mono text-[0.9em]">
            {part.slice(1, -1)}
          </code>
        ) : (
          part
        ),
      )}
    </>
  );
}
