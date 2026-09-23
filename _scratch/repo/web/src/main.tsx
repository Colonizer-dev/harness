import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { loadApi } from "./api";
import { App } from "./App";
import { ApiContext, ToastProvider } from "./context";
import "./index.css";

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
