import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./lib/monaco";
import App from "./App";
import { ErrorBoundary } from "./components/layout/ErrorBoundary";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false, // don't hit the backend on every alt-tab
      retry: false, // a down database should fail fast, not loop
      staleTime: 5 * 60_000, // catalog metadata stays fresh for 5 minutes
    },
  },
});

declare global {
  interface Window {
    /** Set by public/compat.js when this WebKit is too old; it has already rendered an explanation. */
    __vgUnsupported?: boolean;
  }
}

if (!window.__vgUnsupported) ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </QueryClientProvider>
  </React.StrictMode>,
);
