/**
 * One group card: heading + description + a list of buttons. Buttons
 * are disabled while the parent reports a pending action so the user
 * can't trigger a second run before the first settles.
 */
import type React from "react";
import type { ActionGroupSpec } from "../types/desktop.js";

interface ActionGroupProps {
  group: ActionGroupSpec;
  pending: boolean;
  onRun: (action: { label: string; run: () => Promise<unknown> }) => void;
}

export function ActionGroup({ group, pending, onRun }: ActionGroupProps): React.ReactElement {
  return (
    <section className="action-group">
      <div className="section-title">
        <div>
          <h2>{group.title}</h2>
          <p>{group.description}</p>
        </div>
        <span className="badge">{group.actions.length}</span>
      </div>
      <ul className="actions">
        {group.actions.map((action) => (
          <li key={action.label}>
            <button
              type="button"
              title={action.description}
              disabled={pending}
              onClick={() => onRun(action)}
            >
              {action.label}
            </button>
            <small className="action-hint">{action.description}</small>
          </li>
        ))}
      </ul>
    </section>
  );
}
