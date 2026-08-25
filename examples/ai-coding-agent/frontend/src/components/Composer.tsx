import type React from "react";
import { type FormEvent } from "react";

interface ComposerProps {
  value: string;
  running: boolean;
  onChange: (value: string) => void;
  onSubmit: (prompt: string) => void;
}

export function Composer({ value, running, onChange, onSubmit }: ComposerProps): React.ReactElement {
  function handleSubmit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    onSubmit(value);
  }

  return (
    <form onSubmit={handleSubmit}>
      <textarea
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder="描述你想完成的开发任务…"
      />
      <button type="submit" disabled={running}>
        {running ? "运行中" : "发送"}
      </button>
    </form>
  );
}
