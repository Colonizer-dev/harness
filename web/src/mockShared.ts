// Small helpers shared by the in-browser mock (enabled with `?mock=1`). Split out of src/mock.ts
// (issue #827); see that file for the barrel and src/features/<feature>/mock.ts for each feature.
import type { SessionStatus } from "./types";

export const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** A small seeded generator, so the mock's stream has the same uneven rhythm on every run. */
export function rhythm(seed: number): () => number {
  let x = seed || 1;
  return () => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return ((x >>> 0) % 1000) / 1000;
  };
}
export const now = () => new Date().toISOString();
export const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
export const ahead = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString();
export const clone = <T>(value: T): T => structuredClone(value);
export const LIVE: SessionStatus[] = ["starting", "running", "waiting_for_answer", "idle"];
export const isLive = (status: SessionStatus) => LIVE.includes(status);

export interface SocketHandlers {
  open(socket: MockSocket): void;
  message(socket: MockSocket, data: unknown): void;
  close(socket: MockSocket): void;
}

export class MockSocket {
  binaryType: BinaryType = "blob";
  readyState = 0;
  onopen: ((event: Event) => unknown) | null = null;
  onmessage: ((event: MessageEvent) => unknown) | null = null;
  onclose: ((event: CloseEvent) => unknown) | null = null;
  onerror: ((event: Event) => unknown) | null = null;
  private readonly handlers: SocketHandlers;

  constructor(handlers: SocketHandlers) {
    this.handlers = handlers;
    setTimeout(() => {
      if (this.readyState !== 0) return;
      this.readyState = 1;
      this.onopen?.(new Event("open"));
      this.handlers.open(this);
    }, 150);
  }

  deliver(data: string | ArrayBuffer): void {
    if (this.readyState === 1) this.onmessage?.(new MessageEvent("message", { data }));
  }

  send(data: unknown): void {
    if (this.readyState === 1) this.handlers.message(this, data);
  }

  close(): void {
    if (this.readyState >= 2) return;
    this.readyState = 3;
    this.handlers.close(this);
    this.onclose?.(new CloseEvent("close", { code: 1000 }));
  }
}
