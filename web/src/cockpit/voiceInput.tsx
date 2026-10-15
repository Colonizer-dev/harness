// Dictation for a text box, typed or spoken: whichever service the voice module connects (the browser's
// own recognition by default, where words land as they are heard), or a clip recorded here and
// transcribed by the Mothership through OpenAI, Groq, Deepgram, ElevenLabs or a local server. Nothing
// is sent anywhere but the transcription service until the words are sent from the box.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import type { VoiceStatus } from "../types";
import { canRecord, countdown, startRecording, type Recording } from "../voiceRecorder";

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
 * Dictation through the browser's own recogniser. `interim` is what is being heard right now (shown,
 * not yet committed); each final phrase goes to `onFinal`. Stops itself when recognition ends.
 */
function useBrowserDictation(onFinal: (phrase: string) => void) {
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

/**
 * Dictation through a connected service: records a clip, and on stop (or at `maxSeconds`) sends it to
 * the Mothership, which transcribes it with the key it keeps. `onText` gets the transcript;
 * `onFailed` the reason when the service could not be used.
 */
function useServiceDictation({
  maxSeconds,
  transcribe,
  onText,
  onFailed,
}: {
  maxSeconds: number;
  transcribe: (clip: Blob) => Promise<{ text: string }>;
  onText: (text: string) => void;
  onFailed: (reason: string, blocked: boolean) => void;
}) {
  const recording = useRef<Recording | null>(null);
  const [listening, setListening] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const handlers = useRef({ transcribe, onText, onFailed });
  handlers.current = { transcribe, onText, onFailed };

  const stop = useCallback(async () => {
    const r = recording.current;
    recording.current = null;
    setListening(false);
    if (!r) return;
    const clip = await r.stop();
    if (clip.size === 0) return;
    setTranscribing(true);
    try {
      const { text } = await handlers.current.transcribe(clip);
      handlers.current.onText(text);
    } catch (error) {
      handlers.current.onFailed(errorMessage(error), false);
    } finally {
      setTranscribing(false);
    }
  }, []);

  const start = useCallback(async () => {
    if (recording.current) return;
    try {
      const r = await startRecording();
      recording.current = r;
      setElapsed(0);
      setListening(true);
    } catch (error) {
      const blocked = error instanceof DOMException && error.name === "NotAllowedError";
      handlers.current.onFailed(blocked ? "Microphone access was blocked" : errorMessage(error), blocked);
    }
  }, []);

  // The clock behind the countdown, and the cap: at `maxSeconds` the clip is sent as it is.
  useEffect(() => {
    if (!listening) return;
    const began = Date.now();
    const timer = setInterval(() => {
      const ms = Date.now() - began;
      setElapsed(ms);
      if (ms >= maxSeconds * 1000) void stop();
    }, 250);
    return () => clearInterval(timer);
  }, [listening, maxSeconds, stop]);

  useEffect(() => () => recording.current?.cancel(), []);

  return { listening, transcribing, elapsed, start, stop };
}

// --- Voice input ----------------------------------------------------------------------------------

/**
 * The mic behind a text box: whichever service the voice module connects (read while `active`), or
 * the browser's own recognition, which also stands in for the rest of the visit once a service
 * fails. `onHeard` gets each phrase or transcript. Used by the Colonize pane.
 */
export function useVoiceInput({ active, onHeard }: { active: boolean; onHeard: (phrase: string) => void }) {
  const api = useApi();
  const toast = useToast();
  // Which voice service the mic uses: the module's, read while the box is open, unless a service
  // failed this visit, in which case the browser's recogniser stands in until the next reload.
  const [service, setService] = useState<VoiceStatus | null>(null);
  const [fallback, setFallback] = useState(false);
  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    api.voice().then(
      (status) => !cancelled && setService(status),
      () => !cancelled && setService(null),
    );
    return () => {
      cancelled = true;
    };
  }, [api, active]);

  const browserVoice = useBrowserDictation(onHeard);
  const useService = !fallback && service !== null && service.provider !== "browser" && service.configured && canRecord();
  const serviceVoice = useServiceDictation({
    maxSeconds: service?.max_seconds ?? 120,
    transcribe: (clip) => api.transcribe(clip),
    onText: onHeard,
    onFailed: (reason, blocked) => {
      if (!blocked && browserVoice.supported) {
        setFallback(true);
        toast(`${service?.name ?? "Voice service"}: ${reason} — using the browser's recognition for now`, "error");
      } else toast(reason, "error");
    },
  });
  return {
    recording: useService,
    supported: useService || browserVoice.supported,
    listening: useService ? serviceVoice.listening : browserVoice.listening,
    transcribing: useService && serviceVoice.transcribing,
    error: useService ? null : browserVoice.error,
    interim: useService ? "" : browserVoice.interim,
    left: useService && serviceVoice.listening ? countdown(serviceVoice.elapsed, service?.max_seconds ?? 120) : null,
    label: useService && service ? `${service.name}${service.model ? ` · ${service.model}` : ""}` : "the browser's recognition",
    start: () => (useService ? void serviceVoice.start() : browserVoice.start()),
    stop: () => (useService ? void serviceVoice.stop() : browserVoice.stop()),
  };
}

export function MicButton({ listening, busy = false, label, onClick }: { listening: boolean; busy?: boolean; label: string; onClick: () => void }): ReactElement {
  const title = busy ? "Transcribing…" : listening ? "Stop" : `Speak — ${label}`;
  return (
    <button
      type="button"
      aria-label={listening ? "stop listening" : `speak a task (${label})`}
      aria-pressed={listening}
      title={title}
      disabled={busy}
      onClick={onClick}
      className={`mic-button relative grid h-9 w-9 shrink-0 cursor-pointer place-items-center rounded-full border-0 transition-colors disabled:cursor-default disabled:opacity-50 ${listening ? "bg-accent text-on-accent" : "bg-transparent text-muted hover:bg-panel-2 hover:text-text"}`}
    >
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
      </svg>
    </button>
  );
}
