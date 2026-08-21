// Manager plugin frontend. Runs inside the WebView shell that the
// host opens for `com.alex.manager` and calls into the host through
// the standard `window.alex.invoke` transport. From the host's
// point of view, every call is just an `ApiRouter::dispatch` for
// the plugin's own manifest, with the same permission checks as a
// regular app.

const statusEl = document.querySelector("#status");
const listEl = document.querySelector("#apps");
const extStatusEl = document.querySelector("#extension-status");
const extListEl = document.querySelector("#extensions");
const refreshBtn = document.querySelector("#refresh");

function setStatus(text, isError) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", Boolean(isError));
}

function setExtStatus(text, isError) {
  extStatusEl.textContent = text;
  extStatusEl.classList.toggle("error", Boolean(isError));
}

function makeAppRow(app) {
  const li = document.createElement("li");
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = app.name;
  const id = document.createElement("span");
  id.className = "id";
  id.textContent = app.id;
  const version = document.createElement("span");
  version.className = "version";
  version.textContent = `v${app.version}`;
  const actions = document.createElement("span");
  actions.className = "actions";
  const uninstallBtn = document.createElement("button");
  uninstallBtn.type = "button";
  uninstallBtn.textContent = "Uninstall";
  uninstallBtn.addEventListener("click", () => uninstallApp(app.id, uninstallBtn));
  actions.appendChild(uninstallBtn);
  li.append(name, id, version, actions);
  return li;
}

function makeExtRow(ext) {
  const li = document.createElement("li");
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = ext.label;
  const id = document.createElement("span");
  id.className = "id";
  id.textContent = `${ext.kind} · ${ext.id} · ${ext.pluginId}`;
  li.append(name, id);
  return li;
}

async function loadApps() {
  setStatus("Loading…");
  listEl.replaceChildren();
  try {
    const result = await window.alex.invoke("system.listApps", {});
    const apps = Array.isArray(result?.apps) ? result.apps : [];
    if (apps.length === 0) {
      setStatus("No applications installed.");
    } else {
      setStatus(`${apps.length} application(s) installed.`);
      for (const app of apps) {
        listEl.appendChild(makeAppRow(app));
      }
    }
  } catch (error) {
    setStatus(`Failed to list apps: ${error?.message ?? error}`, true);
  }
}

async function loadExtensions() {
  setExtStatus("Loading…");
  extListEl.replaceChildren();
  try {
    const result = await window.alex.invoke("system.listExtensions", {});
    const exts = Array.isArray(result?.extensions) ? result.extensions : [];
    if (exts.length === 0) {
      setExtStatus("No extension points registered.");
    } else {
      setExtStatus(`${exts.length} extension point(s) registered.`);
      for (const ext of exts) {
        extListEl.appendChild(makeExtRow(ext));
      }
    }
  } catch (error) {
    setExtStatus(`Failed to list extensions: ${error?.message ?? error}`, true);
  }
}

async function uninstallApp(id, button) {
  button.disabled = true;
  try {
    await window.alex.invoke("system.uninstall", { id });
    await loadApps();
  } catch (error) {
    setStatus(`Failed to uninstall ${id}: ${error?.message ?? error}`, true);
    button.disabled = false;
  }
}

refreshBtn.addEventListener("click", () => {
  loadApps();
  loadExtensions();
});

// `window.alex.invoke` is unavailable until the host has injected
// the bridge. Wait for it before loading the first batch.
async function waitForBridge() {
  if (window.alex && typeof window.alex.invoke === "function") {
    return;
  }
  await new Promise((resolve) => {
    const check = () => {
      if (window.alex && typeof window.alex.invoke === "function") {
        clearInterval(timer);
        resolve();
      }
    };
    const timer = setInterval(check, 25);
  });
}

(async () => {
  await waitForBridge();
  await Promise.all([loadApps(), loadExtensions()]);
})();
