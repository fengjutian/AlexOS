import { useEffect, useState } from "react";
import { appClient } from "../lib/app-client.js";
import type { AppStatus } from "../types/chat.js";

interface AppStatusState {
  status: AppStatus | null;
  loading: boolean;
  error: string | null;
}

const initial: AppStatusState = { status: null, loading: true, error: null };

export function useAppStatus(): AppStatusState {
  const [state, setState] = useState<AppStatusState>(initial);

  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;
    (async () => {
      try {
        const [info, config] = await Promise.all([
          appClient.info(),
          appClient.getConfig("workspace"),
        ]);
        if (cancelled) return;
        const workspace = typeof config.value === "string" ? config.value : "workspace";
        const status: AppStatus = {
          service: info.service,
          workspace,
          version: info.version,
          node: info.runtime.node,
          uptimeMs: Date.now() - new Date(info.runtime.startedAt).getTime(),
        };
        setState({ status, loading: false, error: null });
      } catch (error) {
        if (cancelled) return;
        const message = error instanceof Error ? error.message : String(error);
        setState({ status: null, loading: false, error: message });
      }
    })();
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  return state;
}
