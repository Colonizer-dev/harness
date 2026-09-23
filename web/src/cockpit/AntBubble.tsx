// One carrier ant's speech bubble, riding the tunnel with it: a rounded panel in the ant's
// tone with a tail pointing down at the ant. Shared by the nest overview and the chamber
// zoom — the caller picks the words (bubbles.ts), this only draws them. Pointer-events-none
// and aria-hidden like the chamber balloons: the ant's own title/aria-label already speaks.
import type { ReactElement } from "react";

export function AntBubble({ text, title, tone }: { text: string; title: string; tone: string }): ReactElement {
  return (
    <span aria-hidden="true" className="pointer-events-none inline-flex flex-col items-center">
      <span
        key={text}
        title={title}
        className="inline-flex items-center whitespace-nowrap nest-glass rounded-md border px-1.5 py-0.5 font-mono text-[10.5px] leading-[14px] text-muted"
        style={{ borderColor: tone, animation: "ck-in 0.4s ease-out" }}
      >
        <span aria-hidden="true" className="mr-1 inline-block h-[7px] w-[7px] shrink-0 rounded-full" style={{ background: tone }} />
        {text}
      </span>
      <span
        aria-hidden="true"
        className="-mt-[5px] block h-[7px] w-[7px] nest-glass-tail rotate-45 border-b border-r"
        style={{ borderColor: tone }}
      />
    </span>
  );
}
