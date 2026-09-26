// A dense archify map for the layout tests: twenty components in columns archify packed tightly
// (one squeezed between two others), four boundaries of which one nests inside another — the shape
// of a real mobile-app map that used to draw as one unreadable knot on a laptop screen.
import type { ArchMap } from "../types";

const c = (id: string, label: string, sublabel: string, x: number, y: number, w = 160, h = 64) => ({
  id,
  type: "backend",
  label,
  sublabel,
  pos: [x, y] as [number, number],
  size: [w, h] as [number, number],
  sources: [{ path: `src/${id}/index.ts` }],
});

export const DENSE_MAP: ArchMap = {
  title: "acme/mobile",
  components: [
    c("user", "App User", "attendee / vendor", 20, 500, 150, 60),
    c("boot", "App Bootstrap", "index.js + App.tsx", 220, 498),
    c("nav", "Navigation", "React Navigation v7", 410, 500),
    c("auth", "Auth", "passcode / biometric", 600, 70),
    c("wallet", "Wallet & Home", "balances, transactions", 600, 250),
    c("gateway", "Payment Gateway", "hosted checkout", 600, 330),
    c("pay", "Payment", "card checkout flow", 600, 430),
    c("qr", "QR Payment", "scan-to-pay", 600, 610),
    c("pos", "POS", "in-person checkout", 600, 790),
    c("storage", "Encrypted Storage", "MMKV + Keychain key", 790, 70, 170),
    c("sync", "Offline Sync Engine", "mutation queue + replay", 790, 710, 170),
    c("bridge", "Terminal Pay Bridge", "card-present tap", 790, 970, 170),
    c("client", "/v1 API Client", "fetch + token refresh", 990, 430, 170),
    c("ws", "WebSocket Client", "realtime events", 990, 610, 170),
    c("legacy", "Legacy SDK", "being retired (#1045)", 990, 790, 170),
    c("backend", "Backend API", "NestJS /api + /v1", 1190, 510, 180, 70),
    c("terminal", "Card Terminal", "card reader SDK", 1190, 970, 180, 60),
    c("push-transport", "Push Messaging", "push transport", 20, 970),
    c("push", "Push Notifications", "notification handlers", 220, 970),
    c("logging", "Logging & Telemetry", "errors, traces, events", 410, 970),
  ],
  connections: [
    { from: "user", to: "boot" },
    { from: "boot", to: "nav" },
    { from: "nav", to: "auth" },
    { from: "nav", to: "wallet" },
    { from: "nav", to: "pay" },
    { from: "nav", to: "qr" },
    { from: "nav", to: "pos" },
    { from: "pay", to: "gateway" },
    { from: "auth", to: "storage" },
    { from: "wallet", to: "client" },
    { from: "pay", to: "client" },
    { from: "qr", to: "ws" },
    { from: "pos", to: "sync" },
    { from: "pos", to: "bridge" },
    { from: "sync", to: "client" },
    { from: "client", to: "backend" },
    { from: "ws", to: "backend" },
    { from: "legacy", to: "backend" },
    { from: "bridge", to: "terminal" },
    { from: "boot", to: "push" },
    { from: "push-transport", to: "push" },
    { from: "boot", to: "logging" },
  ],
  boundaries: [
    { label: "Feature modules (src/features)", wraps: ["auth", "wallet", "pay", "qr", "pos"] },
    { label: "Shared services (src/shared, src/api)", wraps: ["storage", "sync", "bridge", "client", "ws", "legacy"] },
    { label: "Payment & auth trust boundary", wraps: ["auth", "pay", "qr"] },
    { label: "Notifications", wraps: ["push-transport", "push"] },
  ],
};

// The reported map's shape: the entry ("App User", which nothing points at) sits far from the
// centre, at the bottom-left, with a boundary's title between it and the middle of the surface — the
// mothership's corridor used to run from the plot's centre diagonally across that title to reach it.
const m = (id: string, label: string, sublabel: string, x: number, y: number, w = 150, h = 60) => ({
  id,
  type: id === "backend" ? "external" : "backend",
  label,
  sublabel,
  pos: [x, y] as [number, number],
  size: [w, h] as [number, number],
  sources: [{ path: `src/${id}/index.ts` }],
});

export const OFF_CENTRE_ENTRY_MAP: ArchMap = {
  title: "acme/wallet-app",
  components: [
    m("user", "App User", "attendee / vendor", 0, 520),
    m("boot", "App Bootstrap", "index.js + App.tsx", 200, 520),
    m("nav", "Navigation", "React Navigation v7", 400, 520),
    m("auth", "Auth", "passcode / biometric", 600, 40),
    m("wallet", "Wallet & Home", "balances, transactions", 600, 180),
    m("hyperswitch", "Hyperswitch", "hosted checkout", 600, 320),
    m("pay", "Payment", "card checkout flow", 600, 460),
    m("storage", "Encrypted Storage", "MMKV + Keychain key", 820, 120),
    m("client", "/v1 API Client", "fetch + token refresh", 820, 400),
    m("backend", "chi Backend", "NestJS /v1", 1040, 260),
  ],
  connections: [
    { from: "user", to: "boot" },
    { from: "boot", to: "nav" },
    { from: "nav", to: "auth" },
    { from: "nav", to: "wallet" },
    { from: "nav", to: "pay" },
    { from: "pay", to: "hyperswitch" },
    { from: "auth", to: "storage" },
    { from: "wallet", to: "client" },
    { from: "pay", to: "client" },
    { from: "client", to: "backend" },
  ],
  boundaries: [
    { label: "Feature modules (src/features)", wraps: ["auth", "wallet", "hyperswitch", "pay"] },
    { label: "Payment & auth trust boundary", wraps: ["auth", "hyperswitch", "pay"] },
    { label: "Shared services (src/shared)", wraps: ["storage", "client"] },
  ],
};
