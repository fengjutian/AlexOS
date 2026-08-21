// Alex OS Manager plugin (self-hosted form).
//
// In 0.1 the plugin reads ALEX_INSTALL_ROOT directly from the process
// environment and lists installed apps. Reverse IPC (backend asks the
// host a question) lands with the host protocol extension in 0.2, at
// which point the system.manageApps permission becomes enforceable on
// the host side.

const fs = require("node:fs");
const path = require("node:path");

const installRoot = process.env.ALEX_INSTALL_ROOT || "";
const packageRoot = process.env.ALEX_PACKAGE_ROOT || "";

function listApps() {
  if (!installRoot) {
    return [];
  }
  let entries;
  try {
    entries = fs.readdirSync(installRoot, { withFileTypes: true });
  } catch (_error) {
    return [];
  }
  const apps = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.name.startsWith(".")) {
      continue;
    }
    const manifestPath = path.join(installRoot, entry.name, "manifest.json");
    let manifest;
    try {
      manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    } catch (_error) {
      continue;
    }
    apps.push({
      id: manifest.id || entry.name,
      name: manifest.name || entry.name,
      version: manifest.version || "0.0.0",
      path: manifestPath,
    });
  }
  apps.sort((a, b) => a.id.localeCompare(b.id));
  return apps;
}

function emit(kind, payload) {
  process.stdout.write(JSON.stringify({ kind, payload }) + "\n");
}

emit("started", { id: "com.alex.manager", installRoot, packageRoot });

const interval = setInterval(() => {
  const apps = listApps();
  emit("apps", { count: apps.length, apps });
}, 2000);

process.on("SIGTERM", () => {
  clearInterval(interval);
  emit("stopped", { reason: "SIGTERM" });
  process.exit(0);
});

input_lines: {
  // Read stdin to honor the shutdown protocol; keep alive.
  process.stdin.on("data", () => {});
}
