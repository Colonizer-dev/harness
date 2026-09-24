import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { loadApi } from "./api";
import { App } from "./App";
import { ApiContext, ToastProvider } from "./context";
import "./index.css";
import { setupInstallApp } from "./installApp";

// A tab left open across a mothership update still runs the old build, whose lazily loaded chunks
// the new build no longer has. Vite reports that as a preload error: reload once to pick up the new
// build, and not again within a minute, so a genuinely broken asset cannot loop the page.
window.addEventListener("vite:preloadError", (event) => {
  const key = "colonizer:chunk-reload";
  let last = 0;
  try {
    last = Number(sessionStorage.getItem(key) ?? 0);
  } catch {
    // storage blocked: fall through and reload once
  }
  if (Date.now() - last < 60_000) return;
  try {
    sessionStorage.setItem(key, String(Date.now()));
  } catch {
    // ignore
  }
  event.preventDefault();
  window.location.reload();
});

setupInstallApp();

const root = createRoot(document.getElementById("root")!);

loadApi().then((api) => {
  root.render(
    <StrictMode>
      <ApiContext.Provider value={api}>
        <ToastProvider>
          <App />
        </ToastProvider>
      </ApiContext.Provider>
    </StrictMode>,
  );
});
