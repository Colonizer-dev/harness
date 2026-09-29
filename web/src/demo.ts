// The hosted demo build (`vite build --mode demo`, served at colonizer.dev/demo): a constant, so
// the regular build's dead-code elimination drops everything gated on it.
export const DEMO = import.meta.env.MODE === "demo";
