import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef, useState } from "react";
import { SOCKET_OPEN } from "../api";
import { useApi } from "../context";
import { IconRefresh, IconTerminal } from "./icons";
import { Button, Spinner } from "./ui";

function cssVar(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

function terminalTheme() {
  return {
    background: cssVar("--term-bg", "#1c1b18"),
    foreground: cssVar("--term-fg", "#ece9e1"),
    cursor: cssVar("--accent", "#f97316"),
    cursorAccent: cssVar("--term-bg", "#1c1b18"),
    selectionBackground: "rgba(249, 115, 22, 0.35)",
  };
}

type TerminalState = "idle" | "connecting" | "open" | "closed";

export function TerminalPanel({ sessionId, enabled }: { sessionId: string; enabled: boolean }) {
  const api = useApi();
  const hostRef = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<TerminalState>("idle");
  const [exitNote, setExitNote] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    const host = hostRef.current;
    if (!enabled || !host) return;

    let fit: FitAddon | null = null;
    let teardown: (() => void) | null = null;

    const refit = () => {
      try {
        fit?.fit();
      } catch {
        /* host not measurable */
      }
    };

    // Starting while the panel is hidden (e.g. a background tab) would size the shell to a few
    // columns, so wait until the host has real dimensions.
    const start = () => {
      const term = new Terminal({
        cursorBlink: true,
        fontFamily: cssVar("--font-mono", "monospace"),
        fontSize: 13,
        lineHeight: 1.2,
        scrollback: 5000,
        theme: terminalTheme(),
      });
      fit = new FitAddon();
      term.loadAddon(fit);
      term.open(host);
      refit();

      setState("connecting");
      setExitNote(null);
      const ws = api.openTerminal(sessionId, term.cols, term.rows);
      ws.binaryType = "arraybuffer";
      const encoder = new TextEncoder();

      const sendResize = () => {
        if (ws.readyState === SOCKET_OPEN) ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
      };

      ws.onopen = () => {
        setState("open");
        sendResize();
        term.focus();
      };
      ws.onmessage = (event) => {
        if (typeof event.data === "string") {
          try {
            const message = JSON.parse(event.data) as { type?: string; code?: number; message?: string };
            if (message.type === "exit") setExitNote(`Shell exited with code ${message.code ?? "?"}`);
            // The harness reports unreachable VMs as a single error frame before closing.
            if (message.type === "error") setExitNote(message.message || "The terminal could not be opened.");
          } catch {
            /* ignore non-JSON text */
          }
          return;
        }
        term.write(new Uint8Array(event.data as ArrayBuffer));
      };
      ws.onclose = () => setState("closed");
      ws.onerror = () => {};

      const onData = term.onData((data) => {
        if (ws.readyState === SOCKET_OPEN) ws.send(encoder.encode(data));
      });
      const onBinary = term.onBinary((data) => {
        if (ws.readyState !== SOCKET_OPEN) return;
        const bytes = new Uint8Array(data.length);
        for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i) & 0xff;
        ws.send(bytes);
      });
      const onResize = term.onResize(sendResize);

      const scheme = window.matchMedia("(prefers-color-scheme: dark)");
      const onScheme = () => {
        term.options.theme = terminalTheme();
      };
      scheme.addEventListener("change", onScheme);

      teardown = () => {
        scheme.removeEventListener("change", onScheme);
        onData.dispose();
        onBinary.dispose();
        onResize.dispose();
        ws.onclose = null;
        ws.close();
        term.dispose();
      };
    };

    const sized = () => host.clientWidth > 0 && host.clientHeight > 0;
    const observer = new ResizeObserver(() => {
      if (!sized()) return;
      if (teardown) refit();
      else start();
    });
    observer.observe(host);
    if (sized()) start();

    return () => {
      observer.disconnect();
      teardown?.();
      setState("idle");
    };
  }, [api, sessionId, enabled, attempt]);

  return (
    <div className="relative h-full min-h-0 bg-[var(--term-bg)]">
      <div ref={hostRef} className="h-full min-h-0 w-full" />
      {!enabled && (
        <Overlay>
          <IconTerminal size={20} />
          <span>The microVM isn't running, so there is no terminal.</span>
        </Overlay>
      )}
      {enabled && state === "connecting" && (
        <div className="pointer-events-none absolute right-3 top-2 flex items-center gap-1.5 text-[12px] text-[var(--term-fg)] opacity-70">
          <Spinner /> Connecting…
        </div>
      )}
      {enabled && state === "closed" && (
        <Overlay>
          <span>{exitNote ?? "Terminal disconnected."}</span>
          <Button size="sm" onClick={() => setAttempt((n) => n + 1)}>
            <IconRefresh size={13} /> Open a new shell
          </Button>
        </Overlay>
      )}
    </div>
  );
}

function Overlay({ children }: { children: React.ReactNode }) {
  return (
    <div className="absolute inset-0 grid place-items-center bg-[color-mix(in_srgb,var(--term-bg)_82%,transparent)] p-4 text-center">
      <div className="flex flex-col items-center gap-3 text-[13px] text-[var(--term-fg)]">{children}</div>
    </div>
  );
}
