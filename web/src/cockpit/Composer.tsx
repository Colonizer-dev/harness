// The composer: one place to tell Colonizer what to do next, typed or spoken. It floats at the foot of
// the cockpit as a quiet pill ("Describe a task…", ⌘K), opens into a prompt with a repository chip, and
// sends the words as a new colony's instructions — the same open-session launch the Launch view makes,
// nothing invented on top. Voice is the browser's own speech recognition: words land in the prompt as
// they are heard, and nothing is sent until you press send.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { sameOrg, store, stored } from "../components/ui";
import type { Repo, Session } from "../types";

const REPO_KEY = "colonizer.repo";

/** The repositories a prompt can go to: the workspace's own, unarchived, most recently pushed first. */
export function composerRepos(repos: readonly Repo[], org: string | null): Repo[] {
  return repos
    .filter((r) => !r.archived && (!org || sameOrg(r.full_name.split("/")[0], org)))
    .sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? "") || a.full_name.localeCompare(b.full_name));
}

/** The repository the composer starts on: the last one launched on if it is in scope, else the freshest. */
export function defaultRepo(choices: readonly Repo[], remembered: string | null): string | null {
  if (remembered && choices.some((r) => r.full_name === remembered)) return remembered;
  return choices[0]?.full_name ?? null;
}

// --- Speech ------------------------------------------------------------------------------------

interface SpeechResultLike {
  isFinal: boolean;
  0: { transcript: string };
}
interface SpeechEventLike {
  resultIndex: number;
  results: ArrayLike<SpeechResultLike>;
}
interface Recognizer {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: SpeechEventLike) => void) | null;
  onerror: ((event: { error: string }) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
}
type RecognizerCtor = new () => Recognizer;

function recognizerCtor(): RecognizerCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as { SpeechRecognition?: RecognizerCtor; webkitSpeechRecognition?: RecognizerCtor };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

/** Joins what was already typed with what was just heard, with one space between. */
export function appendHeard(text: string, heard: string): string {
  const said = heard.trim();
  if (!said) return text;
  return text.trim() ? `${text.replace(/\s+$/, "")} ${said}` : said;
}

/**
 * Dictation into a text field. `interim` is what is being heard right now (shown, not yet committed);
 * each final phrase goes to `onFinal`. Stops itself when recognition ends.
 */
function useDictation(onFinal: (phrase: string) => void) {
  const ctor = useMemo(recognizerCtor, []);
  const rec = useRef<Recognizer | null>(null);
  const [listening, setListening] = useState(false);
  const [interim, setInterim] = useState("");
  const [error, setError] = useState<string | null>(null);
  const final = useRef(onFinal);
  final.current = onFinal;

  const stop = useCallback(() => {
    rec.current?.stop();
  }, []);

  const start = useCallback(() => {
    if (!ctor || rec.current) return;
    const r = new ctor();
    r.continuous = true;
    r.interimResults = true;
    r.lang = navigator.language || "en-US";
    r.onresult = (event) => {
      let live = "";
      for (let i = event.resultIndex; i < event.results.length; i += 1) {
        const result = event.results[i];
        if (result.isFinal) final.current(result[0].transcript);
        else live += result[0].transcript;
      }
      setInterim(live);
    };
    r.onerror = (event) => {
      setError(event.error === "not-allowed" ? "Microphone access was blocked" : event.error === "no-speech" ? null : `Voice stopped: ${event.error}`);
    };
    r.onend = () => {
      rec.current = null;
      setListening(false);
      setInterim("");
    };
    rec.current = r;
    setError(null);
    setListening(true);
    try {
      r.start();
    } catch {
      rec.current = null;
      setListening(false);
    }
  }, [ctor]);

  useEffect(() => () => rec.current?.stop(), []);

  return { supported: ctor !== null, listening, interim, error, start, stop };
}

// --- The composer -------------------------------------------------------------------------------

export function Composer({
  org,
  repos,
  githubConnected,
  autopilotDefault,
  onCreated,
}: {
  /** The workspace in scope; the repository chip only offers its repositories. */
  org: string | null;
  repos: readonly Repo[];
  githubConnected: boolean;
  autopilotDefault: boolean;
  onCreated: (session: Session) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [picking, setPicking] = useState(false);
  const [query, setQuery] = useState("");
  const choices = useMemo(() => composerRepos(repos, org), [repos, org]);
  const [chosen, setChosen] = useState<string | null>(null);
  const repo = chosen && choices.some((r) => r.full_name === chosen) ? chosen : defaultRepo(choices, stored(REPO_KEY));
  const field = useRef<HTMLTextAreaElement>(null);
  const root = useRef<HTMLDivElement>(null);

  const voice = useDictation((phrase) => setText((current) => appendHeard(current, phrase)));
  const shown = voice.interim ? appendHeard(text, voice.interim) : text;

  // ⌘K / Ctrl+K opens and focuses from anywhere; Escape closes.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setOpen(true);
        requestAnimationFrame(() => field.current?.focus());
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A click outside an empty composer folds it back to the pill.
  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (root.current && !root.current.contains(event.target as Node) && !text.trim() && !voice.listening) {
        setOpen(false);
        setPicking(false);
      }
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [open, text, voice.listening]);

  // The prompt grows with what is in it, up to a point.
  useEffect(() => {
    const el = field.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [shown, open]);

  const openAndFocus = () => {
    setOpen(true);
    requestAnimationFrame(() => field.current?.focus());
  };

  const toggleVoice = () => {
    setOpen(true);
    if (voice.listening) voice.stop();
    else voice.start();
  };

  const canSend = !sending && githubConnected && repo !== null && shown.trim().length > 0;

  const send = async () => {
    if (!canSend || !repo) return;
    if (voice.listening) voice.stop();
    const instructions = shown.trim();
    setSending(true);
    try {
      const session = await api.createSession({ repo, instructions, autopilot: autopilotDefault });
      store(REPO_KEY, repo);
      toast(session.status === "queued" ? `Queued on ${repo} — it starts when a colony finishes` : `Colony launched on ${repo}`);
      setText("");
      setOpen(false);
      onCreated(session);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSending(false);
    }
  };

  const filtered = query.trim() ? choices.filter((r) => r.full_name.toLowerCase().includes(query.trim().toLowerCase())) : choices;
  const placeholder = !githubConnected ? "Connect GitHub in Settings to launch colonies" : voice.listening ? "Listening…" : "Describe a task for a new colony…";

  return (
    <div ref={root} className="pointer-events-none absolute inset-x-0 bottom-5 z-30 flex justify-center px-6">
      <div
        data-open={open}
        data-listening={voice.listening}
        className={`composer v3-pop pointer-events-auto w-full rounded-[22px] border border-border-strong shadow-[0_18px_60px_-12px_rgb(0_0_0/0.55)] transition-[max-width] duration-300 ease-out ${open ? "max-w-[720px]" : "max-w-[460px]"}`}
      >
        {!open ? (
          <div className="flex h-12 items-center gap-2 pl-4 pr-1.5">
            <button
              type="button"
              aria-label="describe a task for a new colony"
              onClick={openAndFocus}
              className="flex min-w-0 flex-1 cursor-text items-center gap-2.5 border-0 bg-transparent p-0 text-left text-[14px] text-faint"
            >
              <Sparkle />
              <span className="truncate">Describe a task for a new colony…</span>
              <kbd className="ml-auto hidden shrink-0 rounded-md border border-border px-1.5 py-0.5 font-mono text-[11px] text-faint sm:inline">⌘K</kbd>
            </button>
            {voice.supported && <MicButton listening={false} onClick={toggleVoice} />}
          </div>
        ) : (
          <div className="p-2">
            <div className="flex items-start gap-2.5 px-2.5 pt-2">
              <span className="mt-[3px]">
                <Sparkle />
              </span>
              <textarea
                ref={field}
                rows={1}
                value={shown}
                disabled={!githubConnected}
                onChange={(event) => setText(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                    event.preventDefault();
                    void send();
                  } else if (event.key === "Escape") {
                    event.preventDefault();
                    if (voice.listening) voice.stop();
                    else setOpen(false);
                  }
                }}
                placeholder={placeholder}
                aria-label="task for a new colony"
                className="bare-field min-h-[26px] flex-1 resize-none border-0 bg-transparent p-0 text-[15px] leading-[1.6] text-text outline-none placeholder:text-faint focus-visible:outline-none"
              />
            </div>

            {voice.listening && <Waveform />}
            {voice.error && <div className="px-3 pt-1 text-[12.5px] text-warn">{voice.error}</div>}

            <div className="mt-2 flex items-center gap-2 px-1">
              <div className="relative min-w-0">
                <button
                  type="button"
                  aria-haspopup="listbox"
                  aria-expanded={picking}
                  aria-label={repo ? `repository · ${repo}` : "choose a repository"}
                  onClick={() => setPicking((p) => !p)}
                  className="flex max-w-[280px] cursor-pointer items-center gap-1.5 rounded-full border border-border bg-transparent px-2.5 py-1 text-[12.5px] text-muted transition-colors hover:border-border-strong hover:text-text"
                >
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M6 3v12M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM18 9a9 9 0 0 1-9 9" />
                  </svg>
                  <span className="truncate font-mono">{repo ?? "choose a repository"}</span>
                </button>
                {picking && (
                  <div role="listbox" aria-label="repositories" className="v3-pop absolute bottom-full left-0 z-40 mb-2 w-[320px] rounded-xl border border-border-strong p-1.5 shadow-[0_16px_48px_rgb(0_0_0/0.4)]">
                    <input
                      autoFocus
                      value={query}
                      onChange={(event) => setQuery(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" && filtered[0]) {
                          event.preventDefault();
                          setChosen(filtered[0].full_name);
                          setPicking(false);
                          setQuery("");
                          field.current?.focus();
                        } else if (event.key === "Escape") setPicking(false);
                      }}
                      placeholder="Find a repository…"
                      aria-label="find a repository"
                      className="bare-field mb-1 w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-[13px] text-text outline-none placeholder:text-faint focus-visible:border-border-strong focus-visible:outline-none"
                    />
                    <div className="scroll-thin max-h-[240px] overflow-y-auto">
                      {filtered.length === 0 && <div className="px-2 py-3 text-[13px] text-faint">No repository matches.</div>}
                      {filtered.map((r) => (
                        <button
                          key={r.full_name}
                          type="button"
                          role="option"
                          aria-selected={r.full_name === repo}
                          onClick={() => {
                            setChosen(r.full_name);
                            setPicking(false);
                            setQuery("");
                            field.current?.focus();
                          }}
                          className={`flex w-full cursor-pointer items-center gap-2 rounded-md border-0 px-2 py-1.5 text-left font-mono text-[12.5px] transition-colors hover:bg-panel-2 ${r.full_name === repo ? "bg-panel-2 text-text" : "bg-transparent text-muted"}`}
                        >
                          <span className="truncate">{r.full_name}</span>
                          {r.private && <span className="ml-auto shrink-0 font-sans text-[11px] text-faint">private</span>}
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </div>
              <span className="hidden text-[12px] text-faint sm:inline">{autopilotDefault ? "autopilot on" : "you review the PR"}</span>
              <div className="flex-1" />
              {voice.supported && <MicButton listening={voice.listening} onClick={toggleVoice} />}
              <button
                type="button"
                aria-label="launch colony"
                title="Launch (Enter)"
                disabled={!canSend}
                onClick={() => void send()}
                className="grid h-9 w-9 cursor-pointer place-items-center rounded-full border-0 bg-accent text-on-accent transition-[opacity,transform] duration-150 hover:brightness-110 active:scale-95 disabled:cursor-default disabled:opacity-35"
              >
                {sending ? (
                  <span aria-hidden="true" className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-current border-t-transparent" />
                ) : (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />
                  </svg>
                )}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function Sparkle(): ReactElement {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" className="shrink-0 text-accent">
      <path d="M12 3.5c.5 3.8 2.7 6 6.5 6.5-3.8.5-6 2.7-6.5 6.5-.5-3.8-2.7-6-6.5-6.5 3.8-.5 6-2.7 6.5-6.5z" fill="currentColor" />
      <path d="M18.5 15.5c.2 1.4 1 2.2 2.5 2.5-1.5.3-2.3 1.1-2.5 2.5-.2-1.4-1-2.2-2.5-2.5 1.5-.3 2.3-1.1 2.5-2.5z" fill="currentColor" opacity="0.6" />
    </svg>
  );
}

function MicButton({ listening, onClick }: { listening: boolean; onClick: () => void }): ReactElement {
  return (
    <button
      type="button"
      aria-label={listening ? "stop listening" : "speak a task"}
      aria-pressed={listening}
      title={listening ? "Stop listening" : "Speak"}
      onClick={onClick}
      className={`composer-mic relative grid h-9 w-9 shrink-0 cursor-pointer place-items-center rounded-full border-0 transition-colors ${listening ? "bg-accent text-on-accent" : "bg-transparent text-muted hover:bg-panel-2 hover:text-text"}`}
    >
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
      </svg>
    </button>
  );
}

/** A soft waveform while the microphone is open — motion that says "I'm listening". */
function Waveform(): ReactElement {
  return (
    <div aria-hidden="true" className="composer-wave flex h-8 items-center justify-center gap-[3px] px-3 pt-1">
      {Array.from({ length: 28 }, (_, i) => (
        <span key={i} style={{ animationDelay: `${(i * 67) % 900}ms` }} />
      ))}
    </div>
  );
}
