import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./styles.css";
import { App } from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { StoreProvider } from "./state/store";
import { QueryProvider } from "./state/query";

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root element");

createRoot(root).render(
  <StrictMode>
    <ErrorBoundary>
      <StoreProvider>
        <QueryProvider>
          <App />
        </QueryProvider>
      </StoreProvider>
    </ErrorBoundary>
  </StrictMode>,
);
