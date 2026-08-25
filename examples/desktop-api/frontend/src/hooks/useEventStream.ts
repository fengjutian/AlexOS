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

export interface UseEventStreamResult {
  events: EventEntry[];
  clear: () => void;
}

export function useEventStream(
  watched: readonly string[],
  cap = DEFAULT_CAP,
): UseEventStreamResult {
  const [events, setEvents] = useState<EventEntry[]>([]);

  useEffect(() => {
    if (watched.length === 0) return undefined;
    const disposers = watched.map((name) =>
      alex.events.on(name, (payload) => {
        setEvents((current) => {
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
  }, [watched, cap]);

  return { events, clear: () => setEvents([]) };
}
