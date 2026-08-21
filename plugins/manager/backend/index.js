// Manager plugin backend. Demonstrates the reverse-IPC contract
// described in `docs/reverse-ipc.md`:
//
// - Send `{kind:"hostCall", id, method, params}` to the host
//   whenever the plugin needs the host to do something on its
//   behalf (e.g. list installed apps, install / uninstall a
//   `.alex` archive).
// - Read host responses from stdin. Each response is a
//   `{kind:"hostResponse", id, result, error}` line. The plugin
//   uses the `id` to correlate responses with the hostCall it
//   previously sent.
//
// This backend is long-lived: it stays running until the host
// terminates it. The host owns the process lifecycle (see
// `plugin::run` in `src/plugin.rs`). Any local `setTimeout` that
// calls `process.exit` is a developer-time smoke signal, gated
// behind `ALEX_REVERSE_IPC_SMOKE_PACKAGE` so it never fires
// during a real `alex manager` / `alex plugin` run.

process.stdin.resume();
process.stdin.setEncoding("utf8");

let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk;
  let idx;
  while ((idx = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, idx);
    buf = buf.slice(idx + 1);
    if (!line) continue;
    process.stdout.write(`hostResponse: ${line}\n`);
  }
});

process.stdout.write("plugin started\n");

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

call("system.listApps", {});
call("system.listExtensions", {});

// Developer-time smoke signal. When set, the backend drives one
// full `system.install` round-trip after a short delay so the
// CI smoke can observe `install_root` actually change. In normal
// use (real WebView sessions, headless `alex plugin` runs) this
// env var is unset and the backend keeps running until the host
// terminates it.
const packagePath = process.env.ALEX_REVERSE_IPC_SMOKE_PACKAGE;
if (packagePath) {
  setTimeout(() => call("system.install", { packagePath }), 100);
  setTimeout(() => call("system.listApps", {}), 300);
}

