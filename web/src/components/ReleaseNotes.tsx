import type { ReactNode } from "react";

// A small, safe Markdown renderer for release notes (issue #1125). It builds React nodes, never
// HTML strings, so nothing in the body can inject markup. Handles headings, bullet lists,
// paragraphs, **bold**, `code` and [text](url) links (http/https only, opened in a new tab).
// HTML comments (docs/release-notes/<tag>.md opens with one) are dropped.

const INLINE = /\*\*([^*]+)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)/g;

function inline(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let key = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1] !== undefined) out.push(<strong key={key++}>{inline(m[1])}</strong>);
    else if (m[2] !== undefined) out.push(<code key={key++}>{m[2]}</code>);
    else if (/^https?:\/\//i.test(m[4])) {
      out.push(
        <a key={key++} className="text-accent hover:underline" href={m[4]} target="_blank" rel="noopener noreferrer">
          {m[3]}
        </a>,
      );
    } else out.push(m[3]);
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function ReleaseNotes({ source, className }: { source: string; className?: string }) {
  const lines = source.replace(/<!--[\s\S]*?-->/g, "").split(/\r?\n/);
  const blocks: ReactNode[] = [];
  let para: string[] = [];
  let items: string[] = [];
  const flush = () => {
    if (para.length) blocks.push(<p key={blocks.length}>{inline(para.join(" "))}</p>);
    if (items.length) {
      blocks.push(
        <ul key={blocks.length} className="list-disc pl-4">
          {items.map((t, i) => (
            <li key={i}>{inline(t)}</li>
          ))}
        </ul>,
      );
    }
    para = [];
    items = [];
  };
  for (const raw of lines) {
    const line = raw.trim();
    const heading = /^#{1,6}\s+(.*)$/.exec(line);
    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (!line) flush();
    else if (heading) {
      flush();
      blocks.push(
        <p key={blocks.length} className="font-semibold text-text">
          {inline(heading[1])}
        </p>,
      );
    } else if (bullet) {
      if (para.length) flush();
      items.push(bullet[1]);
    } else {
      if (items.length) flush();
      para.push(line);
    }
  }
  flush();
  return <div className={className ?? "space-y-1"}>{blocks}</div>;
}
