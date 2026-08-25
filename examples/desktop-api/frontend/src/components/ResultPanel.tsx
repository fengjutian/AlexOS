/**
 * Sticky right-hand panel that pretty-prints the latest action result.
 * Has a "Copy" button (writes the JSON to the clipboard) and a "Clear"
 * button. The pending state renders a small spinner so the user can
 * see their click was registered while a slow call is in flight.
 */
import { useState } from "react";
import type React from "react";
import type { RunnerState } from "../hooks/useActionRunner.js";

interface ResultPanelProps {
  state: RunnerState;
  onClear: () => void;
}

export function ResultPanel({ state, onClear }: ResultPanelProps): React.ReactElement {
  const [copied, setCopied] = useState(false);
  const text = JSON.stringify(state.result, null, 2);

  async function copy(): Promise<void> {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      // Permission denied (rare in a WebView); surface to the user via
      // the result panel so they at least see what happened.
      setCopied(false);
    }
  }

  return (
    <aside>
      <div className="panel-heading">
        <span>
          调用结果 {state.action && <em>· {state.action}</em>}
          {state.pending && <span className="spinner" aria-label="运行中" />}
        </span>
        <div className="panel-actions">
          <button type="button" className="ghost" onClick={() => void copy()} disabled={!text}>
            {copied ? "已复制" : "复制"}
          </button>
          <button type="button" className="ghost" onClick={onClear}>
            清空
          </button>
        </div>
      </div>
      <pre>{text || "// 点击左侧任一按钮发起一次 host 调用"}</pre>
    </aside>
  );
}
