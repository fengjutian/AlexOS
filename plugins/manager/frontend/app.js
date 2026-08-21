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
const installBtn = document.querySelector("#install");
const browseBtn = document.querySelector("#browse");
const packagePathInput = document.querySelector("#package-path");
const installStatusEl = document.querySelector("#install-status");

// Host-side `SignatureState` (kebab-case) → UI label. Anything
// outside this set falls back to `unsigned` so a future host
// addition does not crash the UI.
const SIGNATURE_LABELS = {
  unsigned: "unsigned",
  "signed-trusted": "trusted",
  "signed-untrusted": "untrusted",
  "invalid-signature": "invalid sig",
};
const SIGNATURE_STATES = new Set(Object.keys(SIGNATURE_LABELS));

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
  const sig = document.createElement("span");
  sig.className = "sig-badge";
  const state = SIGNATURE_STATES.has(app.signatureState)
    ? app.signatureState
    : "unsigned";
  sig.dataset.state = state;
  sig.textContent = SIGNATURE_LABELS[state];
  if (state === "invalid-signature") {
    sig.title =
      "Signature metadata is malformed — treat this package as untrusted.";
  } else if (state === "signed-untrusted") {
    sig.title =
      "Signed by a key that is not in the trust store. Review before granting permissions.";
  } else if (state === "signed-trusted") {
    sig.title = "Signed by a publisher in the trust store.";
  } else {
    sig.title = "No signature metadata in the package.";
  }
  const actions = document.createElement("span");
  actions.className = "actions";
  const uninstallBtn = document.createElement("button");
  uninstallBtn.type = "button";
  uninstallBtn.textContent = "Uninstall";
  uninstallBtn.addEventListener("click", () => uninstallApp(app, uninstallBtn));
  actions.appendChild(uninstallBtn);
  li.append(name, id, version, sig, actions);
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

async function uninstallApp(app, button) {
  // Self-protection: never let the manager tear itself down. The
  // plugin id is fixed by the host convention, so a string compare
  // is enough. The Rust side also rejects this, but checking here
  // gives the user an immediate, in-UI error message instead of a
  // round-trip.
  if (app.id === "com.alex.manager") {
    setStatus(
      `Cannot uninstall ${app.name} — the running App Manager cannot remove itself.`,
      true,
    );
    return;
  }
  // Two-step confirmation: first "are you sure", then "also delete
  // the app's private data directory". Both default to "no" so an
  // accidental click does not nuke the install or the data.
  const confirmed = window.confirm(
    `Uninstall ${app.name} (${app.id})?\n\nThe package files will be removed from the system.`,
  );
  if (!confirmed) {
    return;
  }
  const removeData = window.confirm(
    `Also delete ${app.name}'s private data?\n\nThis cannot be undone. Choose "Cancel" to keep the data directory.`,
  );
  button.disabled = true;
  try {
    await window.alex.invoke("system.uninstall", {
      id: app.id,
      removeData,
    });
    await loadApps();
  } catch (error) {
    setStatus(
      `Failed to uninstall ${app.id}: ${error?.message ?? error}`,
      true,
    );
    button.disabled = false;
  }
}

refreshBtn.addEventListener("click", () => {
  loadApps();
  loadExtensions();
});

function setInstallStatus(text, isError) {
  installStatusEl.textContent = text;
  installStatusEl.classList.toggle("error", Boolean(isError));
}

async function installPackage() {
  const raw = packagePathInput.value.trim();
  if (!raw) {
    setInstallStatus("Enter a path to a .alex archive first.", true);
    packagePathInput.focus();
    return;
  }
  if (!/\.alex$/i.test(raw)) {
    setInstallStatus(
      "Path does not end in .alex — make sure you selected the right file.",
      true,
    );
    return;
  }
  // The host's `system.install` defaults to requiring a signature
  // (H2). The user is installing a local archive, so the package
  // is most likely unsigned — ask for an explicit confirmation
  // before we send `requireSignature: false` and bypass the policy.
  // This is the "operator-confirmed unsigned install" path the
  // H2 default carves out.
  const confirmed = window.confirm(
    `Install the package at:\n\n${raw}\n\n` +
      "The host requires a signed package by default. " +
      "Proceed WITHOUT signature verification?\n\n" +
      'Choose "Cancel" to abort and verify the publisher first.',
  );
  if (!confirmed) {
    return;
  }
  installBtn.disabled = true;
  setInstallStatus("Installing…");
  try {
    const result = await window.alex.invoke("system.install", {
      packagePath: raw,
      requireSignature: false,
    });
    setInstallStatus(
      `Installed: ${result?.installed ?? raw} — refreshing the list.`,
    );
    // Keep the path in the input so the user can install the same
    // archive again (e.g. after a failed install where they want to
    // retry) without retyping. Clear only the install-error path
    // below on a confirmed failure.
    await loadApps();
  } catch (error) {
    setInstallStatus(
      `Install failed: ${error?.message ?? error}`,
      true,
    );
  } finally {
    installBtn.disabled = false;
  }
}

installBtn.addEventListener("click", installPackage);
browseBtn.addEventListener("click", browseForPackage);
packagePathInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    installPackage();
  }
});

async function browseForPackage() {
  // The host's `dialog.openFile` IPC is the only way a WebView can
  // surface a real native file picker — WebView2's `<input
  // type="file">` cannot return absolute paths. The manager
  // plugin's manifest declares `dialog.open` so this is a regular
  // permission-checked call.
  browseBtn.disabled = true;
  setInstallStatus("");
  try {
    const result = await window.alex.invoke("dialog.openFile", {
      title: "Select an .alex package",
    });
    if (result?.path) {
      packagePathInput.value = result.path;
      setInstallStatus("");
    }
    // result === { path: null } is the user clicking "Cancel" on
    // the picker — leave the previous value alone and stay quiet.
  } catch (error) {
    setInstallStatus(
      `Could not open the file picker: ${error?.message ?? error}`,
      true,
    );
  } finally {
    browseBtn.disabled = false;
  }
}

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
