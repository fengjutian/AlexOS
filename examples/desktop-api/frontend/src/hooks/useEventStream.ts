/**
 * `useEventStream` — keeps a rolling list of the most recent events
 * emitted by the host. The caller passes the event names it cares about
 * and the hook subscribes once per mount, returning a stable unsubscribe
 * function to clean up.
 *
 * Why a cap: window/clipboard events fire constantly during real use;
 * keeping all of them would push the UI off the page. The cap is 80
 * entries, matching the previous monolithic demo.
 */
import { useEffect, useState } from "react";
import { alex } from "@alex/sdk";
import type { EventEntry } from "../types/desktop.js";

const DEFAULT_CAP = 80;
const clock = () => new Date().toLocaleTimeString();

export function useEventStream(events: readonly string[], cap = DEFAULT_CAP): EventEntry[] {
  const [entries, setEntries] = useState<EventEntry[]>([]);

  useEffect(() => {
    if (events.length === 0) return undefined;
    const disposers = events.map((name) =>
      alex.events.on(name, (payload) => {
        setEntries((current) => {
          const next: EventEntry = { at: clock(), name, payload };
          return [next, ...current].slice(0, cap);
        });
      }),
    );
    return () => {
      for (const dispose of disposers) dispose();
    };
    // The set of subscribed events is allowed to change at runtime; the
    // hook re-subscribes whenever the caller asks for a different set.
  }, [events, cap]);

  return entries;
}
