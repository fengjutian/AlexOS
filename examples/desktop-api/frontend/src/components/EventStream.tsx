/**
 * Event stream panel. Sits under the result panel and shows the last
 * events the host has pushed our way. A "Clear" button wipes the local
 * buffer; the host keeps publishing, so new events will still show up.
 */
import type React from "react";
import type { EventEntry } from "../types/desktop.js";

interface EventStreamProps {
  events: EventEntry[];
  onClear: () => void;
}

export function EventStream({ events, onClear }: EventStreamProps): React.ReactElement {
  return (
    <section className="event-stream">
      <div className="panel-heading events-title">
        <span>事件流</span>
        <button type="button" className="ghost" onClick={onClear}>
          清空
        </button>
      </div>
      {events.length === 0 ? (
        <p className="empty">等待窗口、文件或拖放事件…</p>
      ) : (
        <ol>
          {events.map((entry, index) => (
            <li key={`${entry.at}-${index}-${entry.name}`}>
              <header>
                <b>{entry.name}</b>
                <time>{entry.at}</time>
              </header>
              <pre>{JSON.stringify(entry.payload, null, 2)}</pre>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
