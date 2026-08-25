/**
 * `useActionRunner` — runs an `ActionSpec.run` and stores the latest
 * result plus the action label, so the result panel can show
 * "result of: 写文件". Errors are caught and shaped into the same
 * `{ error, code }` object the host returns, so the UI doesn't have
 * to special-case `try/catch`.
 */
import { useCallback, useState } from "react";

export interface RunnerState {
  /** Action label, used by the result panel as a caption. */
  action: string | null;
  /** Last successful payload or `{ error, code }` for failures. */
  result: unknown;
  /** True between `run` start and its settle. */
  pending: boolean;
}

const initial: RunnerState = { action: null, result: null, pending: false };

export function useActionRunner() {
  const [state, setState] = useState<RunnerState>(initial);

  const run = useCallback(async (label: string, fn: () => Promise<unknown>) => {
    setState({ action: label, result: null, pending: true });
    try {
      const result = await fn();
      setState({ action: label, result, pending: false });
    } catch (error) {
      const detail = error as { message?: string; code?: string };
      setState({
        action: label,
        result: { error: detail?.message ?? String(error), code: detail?.code },
        pending: false,
      });
    }
  }, []);

  const clear = useCallback(() => setState(initial), []);

  return { state, run, clear };
}
