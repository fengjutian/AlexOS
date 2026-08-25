/**
 * Single controlled text input whose value is reused as the demo's
 * "shared payload" (it becomes the saved storage entry, the file
 * contents, the notification body, …). The component is fully
 * stateless — the parent owns the value and the change handler.
 */
import type React from "react";

interface SharedInputProps {
  id?: string;
  label: string;
  hint?: string;
  value: string;
  onChange: (next: string) => void;
}

export function SharedInput({ id, label, hint, value, onChange }: SharedInputProps): React.ReactElement {
  return (
    <label className="shared-input" htmlFor={id}>
      <span className="label-row">
        <b>{label}</b>
        {hint && <small>{hint}</small>}
      </span>
      <input
        id={id}
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        spellCheck={false}
      />
    </label>
  );
}
