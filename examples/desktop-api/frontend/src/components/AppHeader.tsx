/**
 * Top bar: eyebrow tag, title, tagline and a live status pill.
 * Kept dumb (props in / JSX out) so it can be reused on child windows.
 */
import type React from "react";
import type { HostStatus } from "../types/desktop.js";

interface AppHeaderProps {
  status: HostStatus;
  /** Optional runtime identity (transport id, host version) for the right-side caption. */
  runtime: string | null;
}

export function AppHeader({ status, runtime }: AppHeaderProps): React.ReactElement {
  const pill = renderStatusPill(status);
  return (
    <header>
      <div>
        <span className="eyebrow">ALEX RUNTIME · REACT</span>
        <h1>Desktop API Playground</h1>
        <p>在一个标准 React 项目里探索原生桌面能力。</p>
      </div>
      <div className="status-cluster">
        <span className={pill.className}>{pill.text}</span>
        {runtime && <small className="meta">{runtime}</small>}
      </div>
    </header>
  );
}

function renderStatusPill(status: HostStatus): { className: string; text: string } {
  switch (status.state) {
    case "ready":
      return { className: "status", text: "Host 已连接" };
    case "connecting":
      return { className: "status", text: "正在连接 Host…" };
    case "error":
      return { className: "status error", text: `连接失败：${status.message}` };
  }
}
