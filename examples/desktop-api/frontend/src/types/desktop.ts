/**
 * Shared types for the desktop API demo. Kept in one place so every
 * component, hook and the lib wrapper can talk about the same shapes.
 */
import type { AlexEventMap } from "@alex/sdk";

/** Connection state of the underlying Alex runtime. */
export type HostStatus =
  | { state: "connecting" }
  | { state: "ready" }
  | { state: "error"; message: string };

/** A single captured event from `alex.events.on`. */
export interface EventEntry {
  /** Display time (HH:MM:SS). */
  at: string;
  /** Event name as emitted by the host. */
  name: keyof AlexEventMap | string;
  /** Raw payload, kept as `unknown` so the demo can pretty-print anything. */
  payload: unknown;
}

/** Single clickable API action surfaced in the UI. */
export interface ActionSpec {
  /** Visible button label (e.g. "读文件"). */
  label: string;
  /** One-line description shown on hover / under the button. */
  description: string;
  /**
   * Invokes the host. Returning a plain object (not throwing) keeps the
   * UI logic identical for success and "user-friendly skipped" cases.
   */
  run: () => Promise<unknown>;
}

/** A logical group of related actions, rendered as a card. */
export interface ActionGroupSpec {
  /** Group heading (e.g. "文件系统"). */
  title: string;
  /** One-line summary of what the API does. */
  description: string;
  /** Actions in display order. */
  actions: ActionSpec[];
}
