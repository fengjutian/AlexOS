// Reverse-IPC smoke backend. Sends three hostCall envelopes in
// sequence (listApps, listExtensions, install) and waits for
// matching hostResponses on stdin.
//
// `install` is given the absolute path of a pre-packed .alex file
// via the `ALEX_REVERSE_IPC_SMOKE_PACKAGE` env var. This is a
// developer-time smoke signal — the host only sets it from
// `docs/reverse-ipc.md` examples. Production plugin backends
// would obtain the path from a dialog or another trusted source.

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

function call(method, params) {
  process.stdout.write(
    JSON.stringify({
      kind: "hostCall",
      id: `${method}-1`,
      method,
      params: params || {},
    }) + "\n",
  );
}

call("system.listApps", {});
call("system.listExtensions", {});

const packagePath = process.env.ALEX_REVERSE_IPC_SMOKE_PACKAGE;
if (packagePath) {
  setTimeout(() => call("system.install", { packagePath }), 100);
  setTimeout(() => call("system.listApps", {}), 300);
  setTimeout(() => process.exit(0), 2000);
} else {
  setTimeout(() => process.exit(0), 1500);
}

