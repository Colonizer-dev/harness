import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import type { Api } from "./api";
import { ToastStack, toToast, type ToastInput, type ToastItem, type ToastKind } from "./toasts";

export const ApiContext = createContext<Api | null>(null);

export function useApi(): Api {
  const api = useContext(ApiContext);
  if (!api) throw new Error("ApiContext is missing");
  return api;
}

type ToastTone = "info" | "error" | ToastKind;
/** `toast("Saved")`, `toast(message, "error")`, or the richer `toast({ title, body, kind, action })`. */
type PushToast = (message: string | ToastInput, tone?: ToastTone) => void;

const ToastContext = createContext<PushToast>(() => {});

/** How long a leaving toast plays its exit before it is removed. */
const LEAVE_MS = 180;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const push = useCallback<PushToast>((message, tone) => {
    const id = Date.now() + Math.random();
    setToasts((list) => [...list, toToast(id, message, tone)]);
  }, []);
  const dismiss = useCallback((id: number) => {
    setToasts((list) => list.map((t) => (t.id === id ? { ...t, leaving: true } : t)));
    setTimeout(() => setToasts((list) => list.filter((t) => t.id !== id)), LEAVE_MS);
  }, []);
  return (
    <ToastContext.Provider value={push}>
      {children}
      <ToastStack toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

export function useToast(): PushToast {
  return useContext(ToastContext);
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
