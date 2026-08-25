/**
 * Entry point. Wires React 19's `createRoot` API to the #root element
 * declared in `index.html`, then mounts the app inside StrictMode so
 * any lifecycle mistakes trip during development.
 *
 * StrictMode is intentionally a development-only feature: React will
 * double-invoke effects in dev to surface side-effect bugs. The demo's
 * `useEventStream` and `useHostStatus` are written to tolerate that
 * (they bail out via a `cancelled` flag in their async paths).
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App.js";
import "./styles/app.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("index.html is missing the #root element");
}
createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
