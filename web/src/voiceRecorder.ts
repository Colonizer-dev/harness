// Recording a clip for a connected voice service (the voice module): the microphone through
// MediaRecorder, in the first format this browser can write that the Mothership accepts. The clip
// goes to POST /api/voice/transcribe; nothing here talks to a service directly.

/** Formats in order of preference: Opus in WebM (Chrome, Firefox), then MP4/AAC (Safari). */
const FORMATS = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4", "audio/ogg;codecs=opus"];

/** Whether this browser can record at all. */
export function canRecord(): boolean {
  return typeof window !== "undefined" && typeof window.MediaRecorder !== "undefined" && !!navigator.mediaDevices?.getUserMedia;
}

/** The first format `isSupported` accepts, or "" to let the browser choose. */
export function pickFormat(isSupported: (type: string) => boolean): string {
  return FORMATS.find((type) => isSupported(type)) ?? "";
}

/**
 * What the composer shows as the clip nears its cap: the seconds left over the last ten, else
 * nothing. `elapsedMs` is how long it has been recording.
 */
export function countdown(elapsedMs: number, maxSeconds: number): string | null {
  const left = Math.max(0, Math.ceil(maxSeconds - elapsedMs / 1000));
  return left <= 10 ? `${left}s` : null;
}

/** A readable name for where a voice key comes from. */
export function keySourceLabel(source: string | null, keyOptional: boolean): string {
  if (!source) return keyOptional ? "No key set; this server may not need one." : "Not set.";
  if (source === "saved") return "Saved on this machine.";
  if (source.startsWith("provider:")) return `Reused from the ${source.slice("provider:".length)} model provider.`;
  return `Read from ${source}.`;
}

export interface Recording {
  /** Stops and resolves with the clip. */
  stop: () => Promise<Blob>;
  /** Stops and throws the audio away. */
  cancel: () => void;
  /** The microphone stream, so a level meter can listen to the same input. */
  stream: MediaStream;
}

/** Opens the microphone and starts recording. Rejects when access is refused or nothing can record. */
export async function startRecording(): Promise<Recording> {
  if (!canRecord()) throw new Error("This browser can't record audio");
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const mimeType = pickFormat((type) => MediaRecorder.isTypeSupported(type));
  const recorder = mimeType ? new MediaRecorder(stream, { mimeType }) : new MediaRecorder(stream);
  const chunks: Blob[] = [];
  recorder.ondataavailable = (event) => {
    if (event.data.size > 0) chunks.push(event.data);
  };
  const release = () => stream.getTracks().forEach((track) => track.stop());
  recorder.start(250);
  return {
    stream,
    stop: () =>
      new Promise<Blob>((resolve) => {
        recorder.onstop = () => {
          release();
          resolve(new Blob(chunks, { type: recorder.mimeType || mimeType || "audio/webm" }));
        };
        if (recorder.state === "inactive") recorder.onstop(new Event("stop"));
        else recorder.stop();
      }),
    cancel: () => {
      recorder.onstop = release;
      if (recorder.state !== "inactive") recorder.stop();
      else release();
    },
  };
}
