// Shared HTTP transport for the cockpit's typed API clients (issue #827).
// Feature clients under src/features/<feature>/api.ts build on these helpers;
// everything here is exported for them and re-exported from src/api.ts.
import type { ChatStreamEvent } from "./features/chat/types";

/** The part of the WebSocket interface the UI uses, so the mock can stand in for it. */
export type SocketLike = Pick<
  WebSocket,
  "binaryType" | "readyState" | "onopen" | "onmessage" | "onclose" | "onerror" | "send" | "close"
>;

export const SOCKET_OPEN = 1;

export class ApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...(init.headers ?? {}) },
  });
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const message =
      data && typeof data === "object" && "error" in data
        ? String((data as { error: unknown }).error)
        : text || res.statusText;
    throw new ApiError(message, res.status);
  }
  return data as T;
}

export const post = <T>(path: string, body?: unknown) =>
  request<T>(path, { method: "POST", body: body === undefined ? undefined : JSON.stringify(body) });

export const put = <T>(path: string, body: unknown) => request<T>(path, { method: "PUT", body: JSON.stringify(body) });

export const del = <T>(path: string) => request<T>(path, { method: "DELETE" });
export const repoPath = (repo: string) => `/api/repos/${repo.split("/").map(encodeURIComponent).join("/")}`;
/** `?a=1&b=2` from the defined values, or "". */
export const query = (params: Record<string, string | undefined | null>) => {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v != null && v !== "") q.set(k, v);
  const s = q.toString();
  return s ? `?${s}` : "";
};

export const enc = encodeURIComponent;

/** Splits newline-delimited JSON: the complete lines parsed, and the unfinished tail to carry over. */
export function splitNdjson(buffer: string): { events: ChatStreamEvent[]; rest: string } {
  const lines = buffer.split("\n");
  const rest = lines.pop() ?? "";
  const events: ChatStreamEvent[] = [];
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      events.push(JSON.parse(line) as ChatStreamEvent);
    } catch {
      /* a torn line: skipped */
    }
  }
  return { events, rest };
}

/** POSTs a file as the raw body, reporting upload progress (fetch cannot), and answers the JSON reply. */
export function uploadWithProgress<T>(url: string, file: Blob, onProgress?: (fraction: number) => void, signal?: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", url);
    xhr.setRequestHeader("content-type", file.type || "application/octet-stream");
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress?.(e.loaded / e.total);
    };
    xhr.onload = () => {
      let data: unknown = null;
      try {
        data = xhr.responseText ? JSON.parse(xhr.responseText) : null;
      } catch {
        data = xhr.responseText;
      }
      if (xhr.status >= 200 && xhr.status < 300) resolve(data as T);
      else
        reject(
          new ApiError(
            data && typeof data === "object" && "error" in data ? String((data as { error: unknown }).error) : xhr.statusText || `upload failed (${xhr.status})`,
            xhr.status,
          ),
        );
    };
    xhr.onerror = () => reject(new ApiError("the upload failed", 0));
    xhr.onabort = () => reject(new DOMException("aborted", "AbortError"));
    signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(file);
  });
}

/** POSTs `body` and hands each line of the newline-delimited JSON answer to `onEvent` as it lands. */
export async function streamNdjson(url: string, body: unknown, onEvent: (event: ChatStreamEvent) => void, signal?: AbortSignal): Promise<void> {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok || !res.body) {
    const text = await res.text().catch(() => "");
    let message = text || res.statusText;
    try {
      message = String((JSON.parse(text) as { error?: unknown }).error ?? message);
    } catch {
      /* not JSON */
    }
    throw new ApiError(message, res.status);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let rest = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    const split = splitNdjson(rest + decoder.decode(value, { stream: true }));
    rest = split.rest;
    for (const event of split.events) onEvent(event);
  }
  for (const event of splitNdjson(rest + "\n").events) onEvent(event);
}

export function wsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}${path}`;
}
