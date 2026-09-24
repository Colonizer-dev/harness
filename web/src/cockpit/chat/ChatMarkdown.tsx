// A reply rendered as Markdown. Fenced code gets a header (language, copy) and syntax colours from
// the lazily loaded highlighter; inline code that names a repository file opens it in the Code page.
import { isValidElement, memo, useEffect, useState, type ReactElement, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import { useToast } from "../../context";
import { IconCheck, IconCopy, IconExternal } from "../../components/icons";
import { cx } from "../../components/ui";
import { loadHighlighter } from "./highlight";
import { looksLikePath } from "./logic";

function textOf(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(node.props.children);
  return "";
}

export function CopyButton({ text, label = "Copy", className }: { text: string; label?: string; className?: string }): ReactElement {
  const toast = useToast();
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(
          () => {
            setDone(true);
            setTimeout(() => setDone(false), 1400);
          },
          () => toast("Could not copy", "error"),
        );
      }}
      className={cx("inline-flex cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-0.5 text-[11.5px] text-faint hover:bg-panel-2 hover:text-text", className)}
    >
      {done ? <IconCheck size={13} className="text-ok" /> : <IconCopy size={13} />}
      {label === "Copy" && <span>{done ? "Copied" : "Copy"}</span>}
    </button>
  );
}

function CodeBlock({ code, lang, live }: { code: string; lang: string | undefined; live: boolean }): ReactElement {
  const [html, setHtml] = useState<{ for: string; html: string; language: string | null } | null>(null);
  useEffect(() => {
    // While the reply is still streaming the block keeps changing; colour it once it settles.
    if (live) return;
    let cancelled = false;
    void loadHighlighter().then((highlight) => {
      if (cancelled) return;
      const out = highlight(code, lang);
      setHtml({ for: code, html: out.html, language: out.language });
    });
    return () => {
      cancelled = true;
    };
  }, [code, lang, live]);
  const shown = html && html.for === code && html.html ? html : null;
  const language = lang ?? shown?.language ?? null;
  return (
    <div className="my-3 overflow-hidden rounded-xl border border-border bg-panel-2">
      <div className="flex items-center gap-2 border-b border-border px-3 py-1">
        <span className="flex-1 font-mono text-[11px] text-faint">{language ?? "text"}</span>
        <CopyButton text={code} />
      </div>
      <pre className="scroll-thin m-0 overflow-x-auto p-3 font-mono text-[12.5px] leading-relaxed">
        {shown ? <code className="hljs" dangerouslySetInnerHTML={{ __html: shown.html }} /> : <code>{code}</code>}
      </pre>
    </div>
  );
}

export const ChatMarkdown = memo(function ChatMarkdown({
  text,
  live = false,
  onOpenFile,
}: {
  text: string;
  /** Still streaming: code blocks wait to be coloured. */
  live?: boolean;
  onOpenFile?: (path: string) => void;
}): ReactElement {
  return (
    <div className="md chat-md break-words text-[14.5px] leading-[1.65] [overflow-wrap:anywhere]">
      <ReactMarkdown
        components={{
          pre: ({ children }) => {
            const child = Array.isArray(children) ? children[0] : children;
            const className = isValidElement<{ className?: string }>(child) ? child.props.className ?? "" : "";
            const lang = /language-([\w+#.-]+)/.exec(className)?.[1];
            return <CodeBlock code={textOf(children).replace(/\n$/, "")} lang={lang} live={live} />;
          },
          code: ({ children, className }) => {
            const code = textOf(children);
            if (onOpenFile && !className && looksLikePath(code)) {
              const path = code.trim().replace(/:\d+$/, "");
              return (
                <button
                  type="button"
                  onClick={() => onOpenFile(path)}
                  title={`Open ${path} in Code`}
                  className="inline-flex cursor-pointer items-center gap-1 rounded border-0 bg-panel-2 px-1 py-px font-mono text-[0.88em] text-accent hover:underline"
                >
                  {code}
                  <IconExternal size={11} />
                </button>
              );
            }
            return <code className={className}>{children}</code>;
          },
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
});
