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
const servicesStatusEl = document.querySelector("#services-status");
const servicesListEl = document.querySelector("#services");
const trustStatusEl = document.querySelector("#trust-status");
const trustListEl = document.querySelector("#trust");
const auditStatusEl = document.querySelector("#audit-status");
const auditListEl = document.querySelector("#audit");
const hostInfoEl = document.querySelector("#host-info");
const refreshBtn = document.querySelector("#refresh");
const installBtn = document.querySelector("#install");
const browseBtn = document.querySelector("#browse");
const packagePathInput = document.querySelector("#package-path");
const installStatusEl = document.querySelector("#install-status");
const searchInput = document.querySelector("#app-search");
const chipButtons = Array.from(document.querySelectorAll(".filter-chips .chip"));

// Client-side cache of the most recent `system.listApps` result.
// The list is re-rendered (not re-fetched) whenever the search box
// or the state filter chip changes.
const state = {
  allApps: [],
  query: "",
  filter: "all",
};

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

// Map a `RuntimeState` (lowercase enum from `src/runtime.rs`) into a
// human-friendly badge label. Anything outside the known set falls
// back to a neutral string so a future host addition does not break
// the UI.
const RUNTIME_STATE_LABELS = {
  running: "running",
  ready: "ready",
  starting: "starting",
  crashed: "crashed",
  stopped: "stopped",
};
const RUNTIME_STATES = new Set(Object.keys(RUNTIME_STATE_LABELS));

function makeRuntimeBadge(runtime) {
  const wrap = document.createElement("span");
  wrap.className = "runtime-badge";
  if (!runtime) {
    const offline = document.createElement("span");
    offline.className = "runtime-offline";
    offline.textContent = "offline";
    offline.title = "Backend is not running.";
    wrap.appendChild(offline);
    return wrap;
  }
  const state = RUNTIME_STATES.has(runtime.state) ? runtime.state : "stopped";
  wrap.dataset.state = state;
  const badge = document.createElement("span");
  badge.className = "runtime-state";
  badge.textContent = `${runtime.mode ?? "rpc"} · ${RUNTIME_STATE_LABELS[state]}`;
  // Service-mode extras: surface port + readiness so the user can see
  // the proxy is bound and accepting. We never show the per-launch
  // token — that's a host-only secret.
  if (runtime.mode === "service") {
    if (runtime.port) {
      const port = document.createElement("span");
      port.className = "runtime-port";
      port.textContent = `:${runtime.port}`;
      port.title = runtime.ready
        ? "Service backend is accepting connections on this loopback port."
        : "Service backend is bound but has not yet reported ready.";
      wrap.append(badge, port);
    } else {
      wrap.appendChild(badge);
    }
  } else {
    wrap.appendChild(badge);
  }
  if (runtime.pid) {
    const pid = document.createElement("span");
    pid.className = "runtime-pid";
    pid.textContent = `pid ${runtime.pid}`;
    wrap.appendChild(pid);
  }
  if (runtime.lastError) {
    const err = document.createElement("span");
    err.className = "runtime-error";
    err.textContent = "⚠";
    err.title = `Last error: ${runtime.lastError}`;
    wrap.appendChild(err);
  }
  if (Array.isArray(runtime.recentLogs) && runtime.recentLogs.length > 0) {
    const details = document.createElement("details");
    details.className = "runtime-logs";
    const summary = document.createElement("summary");
    summary.textContent = `${runtime.recentLogs.length} log line(s)`;
    details.appendChild(summary);
    const pre = document.createElement("pre");
    pre.textContent = runtime.recentLogs.join("\n");
    details.appendChild(pre);
    wrap.appendChild(details);
  }
  return wrap;
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
  const runtime = makeRuntimeBadge(app.runtime);
  const actions = document.createElement("span");
  actions.className = "actions";
  const uninstallBtn = document.createElement("button");
  uninstallBtn.type = "button";
  uninstallBtn.textContent = "Uninstall";
  uninstallBtn.addEventListener("click", () => uninstallApp(app, uninstallBtn));
  const permBtn = document.createElement("button");
  permBtn.type = "button";
  permBtn.className = "perm-toggle-btn";
  permBtn.textContent = "Permissions…";
  permBtn.addEventListener("click", () => togglePermissions(li, app, permBtn));
  actions.append(permBtn, uninstallBtn);
  li.append(name, id, version, sig, runtime, actions);
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

function runtimeStateOf(app) {
  // `runtime` is absent (skip_serializing_if) for offline apps.
  return app?.runtime?.state ?? "stopped";
}

function appMatchesFilter(app) {
  if (state.filter !== "all" && runtimeStateOf(app) !== state.filter) {
    return false;
  }
  if (!state.query) return true;
  const q = state.query;
  const hay = `${app.name ?? ""} ${app.id ?? ""} ${app.version ?? ""}`.toLowerCase();
  return hay.includes(q);
}

function renderApps() {
  listEl.replaceChildren();
  const matched = state.allApps.filter(appMatchesFilter);
  if (state.allApps.length === 0) {
    setStatus("No applications installed.");
    return;
  }
  if (matched.length === 0) {
    setStatus(
      `0 of ${state.allApps.length} application(s) match the current filter.`,
    );
    return;
  }
  const filterLabel = state.filter === "all" ? "" : ` (${state.filter})`;
  setStatus(
    `${matched.length} of ${state.allApps.length} application(s) shown${filterLabel}.`,
  );
  for (const app of matched) {
    listEl.appendChild(makeAppRow(app));
  }
}

async function loadApps() {
  setStatus("Loading…");
  listEl.replaceChildren();
  try {
    const result = await window.alex.invoke("system.listApps", {});
    state.allApps = Array.isArray(result?.apps) ? result.apps : [];
    renderApps();
    renderServices();
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

// Services section: derived from the same listApps payload, filtered
// to apps with a live runtime (starting / ready / running). The
// `AppSummary.runtime` snapshot already carries mode / port / pid /
// ready / lastError / recentLogs (status.md §2.10), so this is a
// pure view-side re-projection — no extra host call.
const SERVICE_STATES = new Set(["starting", "ready", "running"]);

function renderServices() {
  servicesListEl.replaceChildren();
  const live = state.allApps.filter(
    (app) => app?.runtime && SERVICE_STATES.has(app.runtime.state),
  );
  if (live.length === 0) {
    servicesStatusEl.textContent = "No service-mode runtimes are live.";
    return;
  }
  servicesStatusEl.textContent =
    `${live.length} service(s) running across ${new Set(live.map((a) => a.id)).size} app(s).`;
  for (const app of live) {
    servicesListEl.appendChild(makeServiceRow(app));
  }
}

function makeServiceRow(app) {
  const li = document.createElement("li");
  li.className = "service-row";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = app.name;
  const id = document.createElement("span");
  id.className = "id";
  id.textContent = app.id;
  const mode = document.createElement("span");
  mode.className = "service-mode";
  mode.textContent = app.runtime.mode ?? "rpc";
  const endpoint = document.createElement("span");
  endpoint.className = "service-endpoint";
  endpoint.textContent =
    app.runtime.mode === "service" && app.runtime.port
      ? `127.0.0.1:${app.runtime.port}`
      : "—";
  const state = document.createElement("span");
  state.className = `runtime-badge runtime-state-badge`;
  state.dataset.state = app.runtime.state;
  state.textContent = app.runtime.state;
  const pid = document.createElement("span");
  pid.className = "runtime-pid";
  pid.textContent = app.runtime.pid ? `pid ${app.runtime.pid}` : "";
  li.append(name, id, mode, endpoint, state, pid);
  if (app.runtime.lastError) {
    const err = document.createElement("span");
    err.className = "runtime-error";
    err.title = app.runtime.lastError;
    err.textContent = "⚠";
    li.appendChild(err);
  }
  if (Array.isArray(app.runtime.recentLogs) && app.runtime.recentLogs.length > 0) {
    const details = document.createElement("details");
    details.className = "runtime-logs";
    const summary = document.createElement("summary");
    summary.textContent = `${app.runtime.recentLogs.length} log line(s)`;
    details.appendChild(summary);
    const pre = document.createElement("pre");
    pre.textContent = app.runtime.recentLogs.join("\n");
    details.appendChild(pre);
    li.appendChild(details);
  }
  return li;
}

// ---- Trust store ----------------------------------------------------
//
// Read-only view of the local Trust Store. The host is the only
// authority on which publishers are trusted; the manager plugin
// surfaces the list but cannot add or remove entries (that lives in
// `alex trust …` on the CLI). A fingerprint with a long public-key
// blob is rarely useful in a UI, so we collapse the key into a
// `<details>` and only show the label + fingerprint by default.

async function loadTrustStore() {
  trustStatusEl.textContent = "Loading…";
  trustStatusEl.classList.remove("error");
  trustListEl.replaceChildren();
  try {
    const result = await window.alex.invoke("system.listTrustedPublishers", {});
    const publishers = Array.isArray(result?.publishers)
      ? result.publishers
      : [];
    if (publishers.length === 0) {
      trustStatusEl.textContent = "No publishers trusted yet.";
      return;
    }
    trustStatusEl.textContent = `${publishers.length} trusted publisher(s).`;
    for (const pub of publishers) {
      trustListEl.appendChild(makeTrustRow(pub));
    }
  } catch (error) {
    trustStatusEl.textContent = `Failed to list trust store: ${error?.message ?? error}`;
    trustStatusEl.classList.add("error");
  }
}

function makeTrustRow(pub) {
  const li = document.createElement("li");
  li.className = "trust-row";
  const label = document.createElement("span");
  label.className = "name";
  label.textContent = pub.label;
  const fp = document.createElement("span");
  fp.className = "trust-fingerprint";
  fp.textContent = pub.fingerprint;
  const details = document.createElement("details");
  details.className = "trust-key";
  const summary = document.createElement("summary");
  summary.textContent = "Public key";
  details.appendChild(summary);
  const pre = document.createElement("pre");
  pre.textContent = pub.publicKey;
  details.appendChild(pre);
  li.append(label, fp, details);
  return li;
}

// ---- Audit log -------------------------------------------------------
//
// Read-only view over every app's `*.audit.jsonl` decision log. The
// server returns the entries already sorted newest-first and capped
// to the requested limit; the UI is a flat list because the row
// shape is uniform and a table would just add grid noise for two
// semantic columns. Timestamps are shown as local HH:MM:SS so the
// scrollback stays readable; the raw ms is in the `<time>` title
// attribute for forensic lookups.

async function loadAuditLog() {
  auditStatusEl.textContent = "Loading…";
  auditStatusEl.classList.remove("error");
  auditListEl.replaceChildren();
  try {
    const result = await window.alex.invoke("system.readAuditLog", {
      limit: 200,
    });
    const entries = Array.isArray(result?.entries) ? result.entries : [];
    if (entries.length === 0) {
      auditStatusEl.textContent = "No audit entries yet.";
      return;
    }
    auditStatusEl.textContent = `${entries.length} most recent decision(s) across all apps.`;
    for (const entry of entries) {
      auditListEl.appendChild(makeAuditRow(entry));
    }
  } catch (error) {
    auditStatusEl.textContent = `Failed to read audit log: ${error?.message ?? error}`;
    auditStatusEl.classList.add("error");
  }
}

function makeAuditRow(entry) {
  const li = document.createElement("li");
  li.className = "audit-row";
  const when = document.createElement("time");
  when.className = "audit-when";
  when.textContent = formatTimestamp(entry.timestampMs);
  when.title = new Date(Number(entry.timestampMs) || 0).toISOString();
  const app = document.createElement("span");
  app.className = "audit-app";
  app.textContent = entry.appId;
  const perm = document.createElement("span");
  perm.className = "audit-permission";
  perm.textContent = entry.permission;
  const decision = document.createElement("span");
  decision.className = `audit-decision perm-current perm-current-${entry.decision}`;
  decision.textContent = entry.decision;
  li.append(when, app, perm, decision);
  return li;
}

function formatTimestamp(ms) {
  if (!ms) return "—";
  const d = new Date(Number(ms));
  if (Number.isNaN(d.getTime())) return "—";
  const pad = (n) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

// ---- Host info -------------------------------------------------------
//
// Read-only snapshot of the host process the manager is running in.
// `system.info` is callable by any app (no permission gate), but
// the new `paths` block in the response is only populated for
// plugins — every other app gets `paths: null`. The Manager is a
// plugin, so the four resolved directories light up here.

async function loadHostInfo() {
  hostInfoEl.replaceChildren();
  let info;
  try {
    info = await window.alex.invoke("system.info", {});
  } catch (error) {
    hostInfoEl.appendChild(hostInfoRow("error", `${error?.message ?? error}`));
    return;
  }
  hostInfoEl.appendChild(hostInfoRow("OS", `${info.os} (${info.arch})`));
  hostInfoEl.appendChild(hostInfoRow("Alex version", info.alexVersion ?? "—"));
  hostInfoEl.appendChild(hostInfoRow("IPC protocol", `v${info.protocol}`));
  if (info.paths) {
    hostInfoEl.appendChild(hostInfoRow("Install root", info.paths.installRoot));
    hostInfoEl.appendChild(hostInfoRow("Trust root", info.paths.trustRoot));
    hostInfoEl.appendChild(hostInfoRow("Permissions dir", info.paths.permissionsDir));
    hostInfoEl.appendChild(hostInfoRow("Data root", info.paths.dataDir));
  } else {
    hostInfoEl.appendChild(
      hostInfoRow("paths", "(not exposed to non-plugin callers)"),
    );
  }
}

function hostInfoRow(label, value) {
  const dt = document.createElement("dt");
  dt.className = "host-info-label";
  dt.textContent = label;
  const dd = document.createElement("dd");
  dd.className = "host-info-value";
  dd.textContent = value;
  // Wrap each (label, value) pair so the dt/dd are kept together
  // when CSS grid lays them out.
  const wrap = document.createElement("div");
  wrap.className = "host-info-row";
  wrap.append(dt, dd);
  return wrap;
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

// ---- Permission management -------------------------------------------
//
// Each app row has a "Permissions…" button. Clicking it expands an
// inline panel showing the persisted decisions for that app id, as
// stored in the host's PermissionStore. Each row has three buttons
// (Allow / Deny / Ask) that dispatch `system.setPermission` to flip
// the decision; the new state is re-read on success.
//
// `system.managePermissions` is pre-granted to `com.alex.manager` at
// plugin startup (see `src/webview/shell.rs` and `src/core/plugin.rs`),
// so the dispatch path is allowed without a first-use prompt.
//
// Self-protection: do not let the manager edit its own permission
// store. Editing `com.alex.manager`'s decisions is meaningless (the
// plugin manifest itself declares the permissions it is allowed to
// use) and a misclick could lock the manager out of its own
// `system.*` access on the next launch.

function togglePermissions(li, app, button) {
  // Toggle: if a panel is already open, close it. Otherwise build one.
  const existing = li.querySelector(".perm-panel");
  if (existing) {
    existing.remove();
    button.textContent = "Permissions…";
    return;
  }
  buildPermPanel(li, app);
  button.textContent = "Hide permissions";
}

function buildPermPanel(li, app) {
  const panel = document.createElement("div");
  panel.className = "perm-panel";
  const status = document.createElement("p");
  status.className = "perm-status";
  status.textContent = "Loading…";
  panel.appendChild(status);
  li.appendChild(panel);
  refreshPermPanel(panel, app, status);
}

async function refreshPermPanel(panel, app, statusEl) {
  statusEl.textContent = "Loading…";
  statusEl.classList.remove("error");
  try {
    const result = await window.alex.invoke("system.listPermissions", {
      id: app.id,
    });
    const perms = Array.isArray(result?.permissions) ? result.permissions : [];
    // Clear everything except the status row so the list rebuilds cleanly.
    while (panel.childElementCount > 1) panel.removeChild(panel.lastChild);
    if (perms.length === 0) {
      statusEl.textContent = `No permission decisions yet for ${app.id}. All calls would prompt.`;
      return;
    }
    statusEl.textContent = `${perms.length} persisted decision(s) for ${app.id}.`;
    for (const perm of perms) {
      panel.appendChild(makePermRow(app, perm, panel, statusEl));
    }
  } catch (error) {
    statusEl.textContent = `Failed to list permissions: ${error?.message ?? error}`;
    statusEl.classList.add("error");
  }
}

function makePermRow(app, perm, panel, statusEl) {
  const row = document.createElement("div");
  row.className = "perm-row";
  const name = document.createElement("span");
  name.className = "perm-name";
  name.textContent = perm.name;
  const current = document.createElement("span");
  current.className = `perm-current perm-current-${perm.decision}`;
  current.textContent = perm.decision;
  const actions = document.createElement("span");
  actions.className = "perm-actions";
  for (const decision of ["granted", "denied", "prompt"]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.dataset.decision = decision;
    btn.textContent = labelForDecision(decision);
    if (perm.decision === decision) {
      btn.classList.add("active");
    }
    btn.addEventListener("click", () =>
      setPermDecision(app, perm.name, decision, panel, statusEl, row),
    );
    actions.appendChild(btn);
  }
  row.append(name, current, actions);
  return row;
}

function labelForDecision(decision) {
  return { granted: "Allow", denied: "Deny", prompt: "Ask" }[decision] ?? decision;
}

async function setPermDecision(app, name, decision, panel, statusEl, row) {
  if (app.id === "com.alex.manager") {
    statusEl.textContent = "Cannot edit the running App Manager's own permissions.";
    statusEl.classList.add("error");
    return;
  }
  // Disable all three buttons in the row while the IPC is in flight
  // so a double-click cannot race the audit log.
  for (const btn of row.querySelectorAll("button")) {
    btn.disabled = true;
  }
  try {
    await window.alex.invoke("system.setPermission", {
      id: app.id,
      name,
      decision,
    });
    await refreshPermPanel(panel, app, statusEl);
  } catch (error) {
    statusEl.textContent = `Failed to set ${name}: ${error?.message ?? error}`;
    statusEl.classList.add("error");
    for (const btn of row.querySelectorAll("button")) {
      btn.disabled = false;
    }
  }
}

refreshBtn.addEventListener("click", () => {
  loadApps();
  loadExtensions();
  loadTrustStore();
  loadAuditLog();
  loadHostInfo();
});

// Search box: case-insensitive substring match against name/id/version.
// Re-renders from the cached list — does not re-hit the host.
searchInput.addEventListener("input", () => {
  state.query = searchInput.value.trim().toLowerCase();
  renderApps();
});

// Filter chips: tab-style toggle. Only one active at a time. The
// "all" chip is the default; clicking a state chip filters by
// `runtime.state`. Apps without a runtime (offline) only match
// under "all" or "stopped".
for (const chip of chipButtons) {
  chip.addEventListener("click", () => {
    state.filter = chip.dataset.filter;
    for (const other of chipButtons) {
      const active = other === chip;
      other.classList.toggle("active", active);
      other.setAttribute("aria-selected", String(active));
    }
    renderApps();
  });
}

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
  await Promise.all([
    loadApps(),
    loadExtensions(),
    loadTrustStore(),
    loadAuditLog(),
    loadHostInfo(),
  ]);
})();
