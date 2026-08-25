/**
 * `useHostStatus` — opens the demo by pinging `system.capabilities` once
 * on mount. We treat the call as the host handshake: success → "ready",
 * rejection → "error", still in flight → "connecting".
 *
 * The capabilities payload is also returned so the UI can show the
 * runtime identity, OS, and which capabilities the host actually
 * advertised.
 */
import { useEffect, useState } from "react";
import { desktop } from "../lib/desktop.js";
import type { HostStatus } from "../types/desktop.js";

export interface UseHostStatusResult {
  status: HostStatus;
  /** Capabilities payload from the host, populated only when `ready`. */
  capabilities: unknown | null;
}

export function useHostStatus(): UseHostStatusResult {
  const [status, setStatus] = useState<HostStatus>({ state: "connecting" });
  const [capabilities, setCapabilities] = useState<unknown | null>(null);

  useEffect(() => {
    let cancelled = false;
    desktop.system
      .capabilities()
      .then((payload) => {
        if (cancelled) return;
        setCapabilities(payload);
        setStatus({ state: "ready" });
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        const message = error instanceof Error ? error.message : String(error);
        setStatus({ state: "error", message });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return { status, capabilities };
}
