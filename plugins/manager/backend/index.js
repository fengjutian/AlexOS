// Manager plugin backend.
//
// Two execution paths reach this file:
//
// 1. Webview mode — `alex manager` (self-hosted) or
//    `alex plugin <id>` (without `--headless`). The host opens a
//    WebView2 window and supervises the backend through
//    `RuntimeHandle` (a separate `runtime_manager` thread). The
//    frontend talks to the host directly through
//    `window.alex.invoke("system.listApps", {})` etc.; the host's
//    `ApiRouter` enforces the plugin's `system.*` permissions on
//    every call. The backend in this mode is mostly idle: it
//    stays alive so the host has a process to supervise, and
//    logs the readiness line below. The reverse-IPC `hostCall`
//    envelopes it writes are harmless because the
//    `RuntimeManager` only reads the backend's stdout when the
//    frontend explicitly calls `runtime.invoke` — and the manager
//    UI does not do that in 0.1.
//
// 2. Headless mode — `alex plugin <id> --headless`. The host's
//    `plugin::run` takes over the backend's stdin/stdout and runs
//    `run_unified_dispatch` in a background thread, parsing each
//    line as a `hostCall` envelope, dispatching through the
//    plugin's own `ApiRouter`, and writing the matching
//    `hostResponse` back. In this mode the `hostCall` envelopes
//    below are actually useful — the host dispatches them, the
//    host writes a `hostResponse` line to our stdin, and we log
//    it so the smoke harness can see the round-trip completed.

process.stdin.resume();
process.stdin.setEncoding("utf8");

process.stdin.on("data", (chunk) => {
  // Forward the host's `hostResponse` envelopes verbatim to our
  // stdout. The host's `run_unified_dispatch` reads our stdout
  // line-by-line: lines that parse as `{kind:"hostCall", ...}` get
  // dispatched; everything else is treated as free-form log output
  // and echoed to the host terminal unchanged. Re-emitting the
  // hostResponse lines (with the original `{kind:"hostResponse", ...}`
  // shape intact) lets the smoke harness see the round-trip in
  // headless mode without breaking the parser.
  process.stdout.write(chunk);
});

process.stdout.write("manager backend ready\n");

let nextId = 1;
function call(method, params) {
  const id = `${method}-${nextId++}`;
  process.stdout.write(
    JSON.stringify({
      kind: "hostCall",
      id,
      method,
      params: params || {},
    }) + "\n",
  );
  return id;
}

// Reverse-IPC smoke lines. In webview mode these are dead
// letters; in headless mode they exercise the hostCall/hostResponse
// round-trip described in `docs/reverse-ipc.md`.
call("system.listApps", {});
call("system.listExtensions", {});

// Developer-time smoke signal. When set, the backend drives one
// full `system.install` round-trip after a short delay so the
// CI smoke can observe `install_root` actually change. The host
// only sets this from automated smoke harnesses; in normal use
// (real WebView sessions, headless `alex plugin` runs) this env
// var is unset.
const packagePath = process.env.ALEX_REVERSE_IPC_SMOKE_PACKAGE;
if (packagePath) {
  setTimeout(() => call("system.install", { packagePath }), 100);
  setTimeout(() => call("system.listApps", {}), 300);
}


