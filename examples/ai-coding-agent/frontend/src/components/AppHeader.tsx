import type React from "react";
import type { AppStatus } from "../types/chat.js";

interface AppHeaderProps {
  status: AppStatus | null;
  loading: boolean;
  error: string | null;
  runtimeStatus: string;
}

export function AppHeader({ status, loading, error, runtimeStatus }: AppHeaderProps): React.ReactElement {
  return (
    <header>
      <div>
        <span className="eyebrow">ALEX RUNTIME</span>
        <h1>Coding Agent</h1>
      </div>
      <div className="status-group">
        <span className="status">{runtimeStatus}</span>
        <small className="meta">
          {loading && "loading service…"}
          {error && `service: ${error}`}
          {status && !error && (
            <>
              {status.service}@{status.version} · node {status.node} · {status.workspace}
            </>
          )}
        </small>
      </div>
    </header>
  );
}
