// App Manager UI — vanilla JS front-end. Wires the `window.alex`
// bridge (see `manager_webview.rs`) to a list + detail view backed by
// the `manager.*` IPC methods on `ManagerRouter`. No build step: this
// file is served from `alex://system/app-manager/manager_app.js`.
//
// IPC method names + payload shapes (camelCase) match
// `src/core/manager.rs::dispatch_authorized`. Any new method added
// there should be reflected here.

(() => {
  "use strict";

  const alex = window.alex;
  if (!alex) {
    document.body.textContent =
      "Manager bridge unavailable (window.alex is missing).";
    return;
  }

  // -- State ----------------------------------------------------------------
  let currentApps = [];
  let currentDetail = null;
  let currentServices = [];
  let currentPermissions = [];
  let searchQuery = "";

  // -- DOM refs -------------------------------------------------------------
  const $ = (id) => document.getElementById(id);
  const listView = $("list-view");
  const detailView = $("detail-view");
  const aiView = $("ai-view");
  const appTable = $("app-table");
  const appTbody = $("app-tbody");
  const emptyState = $("empty-state");
  const listError = $("list-error");
  const searchInput = $("search");
  const installBtn = $("install-btn");
  const installFile = $("install-file");
  const backBtn = $("back-btn");
  const toastEl = $("toast");
  const modalEl = $("modal");
  const modalMessageEl = $("modal-message");
  const modalCancelEl = $("modal-cancel");
  const modalConfirmEl = $("modal-confirm");

  function showApplicationsView() {
    aiView.hidden = true;
    detailView.hidden = true;
    listView.hidden = false;
  }

  async function showAiView() {
    listView.hidden = true;
    detailView.hidden = true;
    aiView.hidden = false;
    await loadAiOverview();
  }

  async function loadAiOverview() {
    const error = $("ai-error");
    error.hidden = true;
    try {
      const overview = await call("manager.ai_overview");
      renderAiOverview(overview ?? {});
    } catch (cause) {
      error.textContent = `AI Runtime is unavailable: ${cause?.message ?? cause}`;
      error.hidden = false;
      $("ai-providers").textContent = "";
      $("ai-mcp").textContent = "";
      $("ai-agents").textContent = "";
    }
  }

  function renderAiOverview(overview) {
    const health = new Map((overview.providerHealth ?? []).map((item) => [item.id, item]));
    const providers = overview.providers ?? [];
    $("ai-providers").innerHTML = providers.length
      ? providers.map((provider) => {
          const state = health.get(provider.id)?.status ?? (provider.enabled ? "unknown" : "disabled");
          return `<div class="service-row"><div><div class="name">${escapeText(provider.id)}</div><div class="meta">${escapeText(provider.kind)} · ${escapeText(provider.endpoint)}</div></div><div class="meta">secret: ${health.get(provider.id)?.secretConfigured ? "configured" : "missing"}</div><span class="badge ${stateClass(state)}">${escapeText(formatState(state))}</span><button type="button" data-ai-action="provider.remove" data-provider-id="${escapeText(provider.id)}">Remove</button></div>`;
        }).join("")
      : '<p class="muted">No remote model providers configured.</p>';

    const applications = overview.applications ?? [];
    const connections = applications.flatMap((app) => {
      const values = Array.isArray(app.mcp) ? app.mcp : (app.mcp?.connections ?? []);
      return values.map((connection) => ({ ...connection, appId: app.id, appName: app.name }));
    });
    $("ai-mcp").innerHTML = connections.length
      ? connections.map((connection) => `<div class="service-row"><div><div class="name">${escapeText(connection.binding)}</div><div class="meta">${escapeText(connection.appName ?? connection.application)}</div></div><span class="badge">${escapeText(connection.era ?? "modern")}</span><button type="button" data-ai-action="mcp.disconnect" data-app-id="${escapeText(connection.appId ?? connection.application)}" data-binding="${escapeText(connection.binding)}">Disconnect</button></div>`).join("")
      : '<p class="muted">No live MCP connections.</p>';

    const runs = applications.flatMap((app) => (app.agents?.runs ?? []).map((run) => ({ ...run, appId: app.id, appName: app.name })));
    $("ai-agents").innerHTML = runs.length
      ? runs.map((run) => `<div class="service-row"><div><div class="name">${escapeText(run.id)}</div><div class="meta">${escapeText(run.appName)} · step ${Number(run.step ?? 0)}</div></div><span class="badge ${stateClass(run.state)}">${escapeText(formatState(run.state))}</span><div><button type="button" data-ai-action="agent.pause" data-app-id="${escapeText(run.appId)}" data-run-id="${escapeText(run.id)}">Pause</button><button type="button" data-ai-action="agent.resume" data-app-id="${escapeText(run.appId)}" data-run-id="${escapeText(run.id)}">Resume</button><button type="button" data-ai-action="agent.cancel" data-app-id="${escapeText(run.appId)}" data-run-id="${escapeText(run.id)}">Cancel</button></div></div>`).join("")
      : '<p class="muted">No Agent runs.</p>';
  }

  // -- IPC helper -----------------------------------------------------------
  // `window.alex.invoke(method, params)` already returns a promise that
  // resolves with `result` or rejects with `{ code, message }`. We just
  // need a small wrapper that unwraps the data field the Manager router
  // adds (most methods return `{ "field": value }`).
  async function call(method, params) {
    const result = await alex.invoke(method, params ?? {});
    if (result && typeof result === "object" && "ok" in result && result.ok) {
      return result;
    }
    return result;
  }

  // -- Toast ----------------------------------------------------------------
  let toastTimer = null;
  function toast(message, kind = "info", ms = 3500) {
    toastEl.textContent = message;
    toastEl.className = "toast " + kind;
    toastEl.hidden = false;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toastEl.hidden = true; }, ms);
  }

  function showError(prefix, error) {
    const code = error && error.code ? `[${error.code}] ` : "";
    toast(`${prefix}: ${code}${error?.message ?? error}`, "error", 6000);
  }

  // -- Confirm modal --------------------------------------------------------
  function confirmModal(message, confirmLabel = "Confirm", danger = true) {
    return new Promise((resolve) => {
      modalMessageEl.textContent = message;
      modalConfirmEl.textContent = confirmLabel;
      modalConfirmEl.classList.toggle("danger", danger);
      modalConfirmEl.classList.toggle("confirm", danger);
      modalEl.hidden = false;

      const cleanup = (result) => {
        modalEl.hidden = true;
        modalConfirmEl.removeEventListener("click", onConfirm);
        modalCancelEl.removeEventListener("click", onCancel);
        document.removeEventListener("keydown", onKey);
        resolve(result);
      };
      const onConfirm = () => cleanup(true);
      const onCancel = () => cleanup(false);
      const onKey = (event) => {
        if (event.key === "Escape") onCancel();
        else if (event.key === "Enter") onConfirm();
      };

      modalConfirmEl.addEventListener("click", onConfirm);
      modalCancelEl.addEventListener("click", onCancel);
      document.addEventListener("keydown", onKey);
    });
  }

  // -- Formatting helpers ---------------------------------------------------
  function escapeText(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    }[c]));
  }

  function formatState(state) {
    if (!state) return "—";
    return String(state).replace(/-/g, " ");
  }

  function stateClass(value) {
    return "state-" + String(value).toLowerCase();
  }

  function formatSource(source) {
    if (!source) return "—";
    return source.replace(/-/g, " ");
  }

  function formatDate(iso) {
    if (!iso) return null;
    const date = new Date(iso);
    if (isNaN(date.getTime())) return iso;
    return date.toLocaleString();
  }

  // -- List view ------------------------------------------------------------
  async function loadList() {
    listError.hidden = true;
    try {
      const result = await call("manager.list_apps");
      currentApps = Array.isArray(result?.apps) ? result.apps : [];
    } catch (error) {
      listError.textContent = `Failed to list apps: ${
        error?.message ?? error
      }`;
      listError.hidden = false;
      currentApps = [];
    }
    renderList();
  }

  function renderList() {
    const q = searchQuery.trim().toLowerCase();
    const filtered = q
      ? currentApps.filter((app) => {
          return (
            app.id?.toLowerCase().includes(q) ||
            app.name?.toLowerCase().includes(q) ||
            app.runtime?.state?.toLowerCase().includes(q)
          );
        })
      : currentApps;

    if (currentApps.length === 0) {
      appTable.hidden = true;
      emptyState.hidden = false;
      appTbody.innerHTML = "";
      return;
    }
    if (filtered.length === 0) {
      appTable.hidden = true;
      emptyState.hidden = false;
      emptyState.firstElementChild.textContent =
        "No apps match the current filter.";
      appTbody.innerHTML = "";
      return;
    }
    emptyState.hidden = true;
    appTable.hidden = false;
    emptyState.firstElementChild.textContent = "No installed applications.";

    appTbody.innerHTML = filtered
      .map((app) => {
        const state = app.runtime?.state ?? "stopped";
        const sig = app.signatureState ?? "unsigned";
        const source = app.installSource ?? "local-package";
        return `
          <tr data-id="${escapeText(app.id)}">
            <td>
              <span class="app-name">${escapeText(app.name)}</span>
              <span class="app-id">${escapeText(app.id)}</span>
            </td>
            <td>${escapeText(app.version ?? "—")}</td>
            <td>
              <span class="badge ${stateClass(state)}">${escapeText(
          formatState(state),
        )}</span>
              ${
                app.runtime?.ready
                  ? '<span class="badge state-healthy" title="alex.ready handshake observed">ready</span>'
                  : ""
              }
            </td>
            <td>
              <span class="badge source-${escapeText(source)}">${escapeText(
          formatSource(source),
        )}</span>
            </td>
            <td>
              <span class="badge sig-${escapeText(sig)}">${escapeText(
          sig.replace(/-/g, " "),
        )}</span>
            </td>
            <td class="actions-col">
              <button type="button" data-row-action="launch" ${
                app.runtime?.state && app.runtime.state !== "stopped"
                  ? "disabled"
                  : ""
              }>Launch</button>
              <button type="button" data-row-action="stop" ${
                app.runtime?.state && app.runtime.state !== "stopped"
                  ? ""
                  : "disabled"
              }>Stop</button>
            </td>
          </tr>`;
      })
      .join("");

    for (const row of appTbody.querySelectorAll("tr")) {
      const id = row.dataset.id;
      row.addEventListener("click", (event) => {
        // Clicks on the action buttons stay on the row; clicks
        // anywhere else drill into the detail view.
        if (event.target.closest("button")) return;
        navigate(`#/app/${encodeURIComponent(id)}`);
      });
      for (const button of row.querySelectorAll("button[data-row-action]")) {
        button.addEventListener("click", async (event) => {
          event.stopPropagation();
          const action = button.dataset.rowAction;
          try {
            if (action === "launch") {
              await call("manager.launch", { id });
              toast(`${id}: launch requested`, "success");
            } else if (action === "stop") {
              await call("manager.stop", { id });
              toast(`${id}: stop requested`, "success");
            }
            await loadList();
          } catch (error) {
            showError(`${id} ${action}`, error);
          }
        });
      }
    }
  }

  // -- Detail view ----------------------------------------------------------
  async function loadDetail(id) {
    detailView.hidden = false;
    listView.hidden = true;
    $("detail-name").textContent = "Loading…";
    $("detail-id").textContent = id;
    $("detail-version").textContent = "";
    $("detail-source").textContent = "";
    $("detail-description").hidden = true;
    $("detail-path").textContent = "";
    $("detail-last-launched").hidden = true;
    $("detail-signature").textContent = "";
    $("detail-runtime").textContent = "";
    $("services").innerHTML = "";
    $("permissions").innerHTML = "";
    $("audit").innerHTML = "";
    $("logs").textContent = "";

    try {
      const [details, services, runtime] = await Promise.all([
        call("manager.get_app", { id }),
        call("manager.list_services", { id }).catch(() => ({ services: [] })),
        call("manager.runtime_status", { id }).catch(() => null),
      ]);
      currentDetail = details;
      currentServices = Array.isArray(services?.services)
        ? services.services
        : [];
      renderDetail(details, runtime);
      loadPermissions(id);
      loadAuditLog(id);
    } catch (error) {
      currentDetail = null;
      currentServices = [];
      $("detail-name").textContent = "Not available";
      $("detail-id").textContent = id;
      const code = error?.code ? `[${error.code}] ` : "";
      showError(`Failed to load ${id}`, error);
      $("services").innerHTML = `<p class="muted">${escapeText(
        code + (error?.message ?? error),
      )}</p>`;
    }
  }

  function renderDetail(details, runtime) {
    const id = details.id ?? details.summary?.id ?? "";
    $("detail-name").textContent = details.name ?? "(unnamed)";
    $("detail-id").textContent = id;
    $("detail-version").textContent = "v" + (details.version ?? "?");
    $("detail-source").textContent = formatSource(details.installSource);
    if (details.description) {
      $("detail-description").textContent = details.description;
      $("detail-description").hidden = false;
    }
    $("detail-path").textContent = details.path ?? details.installPath ?? "";
    const lastLaunched = formatDate(details.lastLaunchedAt);
    if (lastLaunched) {
      $("detail-last-launched").textContent = ` · last launched ${lastLaunched}`;
      $("detail-last-launched").hidden = false;
    }
    const sig = details.signatureState ?? "unsigned";
    const sigEl = $("detail-signature");
    sigEl.textContent = sig.replace(/-/g, " ");
    sigEl.className = "badge sig-" + sig;

    const live = runtime ?? details.runtime ?? null;
    const state = live?.state ?? "stopped";
    const runtimeEl = $("detail-runtime");
    runtimeEl.textContent = formatState(state) + (live?.ready ? " · ready" : "");
    runtimeEl.className = "badge " + stateClass(state);

    renderServices();
    renderLogs(details, live);
  }

  function renderServices() {
    const container = $("services");
    if (!currentServices.length) {
      container.innerHTML = `<p class="muted">No declared services (v1 single-backend app — use Launch/Stop on the toolbar above).</p>`;
      return;
    }
    container.innerHTML = currentServices
      .map((svc) => {
        const status = svc.status ?? "pending";
        const restart = svc.restartCount ?? 0;
        return `
          <div class="service-row" data-service="${escapeText(svc.name)}">
            <div>
              <div class="name">${escapeText(svc.name)}</div>
              ${
                svc.lastError
                  ? `<div class="meta">${escapeText(svc.lastError)}</div>`
                  : ""
              }
            </div>
            <div class="meta">restarts: ${restart}</div>
            <div>
              <span class="badge ${stateClass(status)}">${escapeText(
          formatState(status),
        )}</span>
            </div>
            <div class="actions">
              <button type="button" data-svc-action="start">Start</button>
              <button type="button" data-svc-action="stop">Stop</button>
              <button type="button" data-svc-action="restart">Restart</button>
            </div>
          </div>`;
      })
      .join("");

    for (const row of container.querySelectorAll(".service-row")) {
      const service = row.dataset.service;
      for (const button of row.querySelectorAll("button[data-svc-action]")) {
        button.addEventListener("click", async () => {
          const action = button.dataset.svcAction;
          const map = {
            start: "manager.start_service",
            stop: "manager.stop_service",
            restart: "manager.restart_service",
          };
          try {
            await call(map[action], { id: currentDetail.id, service });
            toast(`${currentDetail.id}/${service}: ${action} requested`, "success");
            await refreshDetail();
          } catch (error) {
            showError(`${service} ${action}`, error);
          }
        });
      }
    }
  }

  function renderLogs(details, runtime) {
    const logs = [];
    if (Array.isArray(runtime?.logs)) logs.push(...runtime.logs);
    if (Array.isArray(details?.runtime?.recentLogs))
      logs.push(...details.runtime.recentLogs);
    // Deduplicate while preserving order (the two arrays may overlap
    // when the user re-fetches and the snapshot has the same tail).
    const seen = new Set();
    const unique = logs.filter((line) => {
      if (seen.has(line)) return false;
      seen.add(line);
      return true;
    });
    $("logs").textContent = unique.slice(-50).join("\n");
  }

  async function loadPermissions(id) {
    try {
      const result = await call("manager.permissions", { id });
      currentPermissions = Array.isArray(result?.permissions)
        ? result.permissions
        : [];
    } catch (error) {
      currentPermissions = [];
      showError(`Failed to load permissions for ${id}`, error);
    }
    renderPermissions();
  }

  function renderPermissions() {
    const container = $("permissions");
    if (!currentPermissions.length) {
      container.innerHTML = `<p class="muted">No declared permissions.</p>`;
      return;
    }
    container.innerHTML = currentPermissions
      .map((perm) => {
        const decision = perm.decision ?? "prompt";
        return `
          <div class="permission-row" data-permission="${escapeText(perm.name)}">
            <div>
              <div class="name">${escapeText(perm.name)}</div>
              ${
                perm.manifestDeclared === false
                  ? '<div class="meta">Not declared in manifest</div>'
                  : ""
              }
            </div>
            <div class="decision-group" role="radiogroup" aria-label="Permission decision">
              ${["granted", "prompt", "denied"]
                .map(
                  (value) => `
                <button type="button"
                  data-decision="${value}"
                  class="${decision === value ? "active" : ""}"
                  role="radio"
                  aria-checked="${decision === value}">
                  ${value}
                </button>`,
                )
                .join("")}
            </div>
          </div>`;
      })
      .join("");

    for (const row of container.querySelectorAll(".permission-row")) {
      const permission = row.dataset.permission;
      for (const button of row.querySelectorAll("button[data-decision]")) {
        button.addEventListener("click", async () => {
          const decision = button.dataset.decision;
          try {
            await call("manager.set_permission", {
              id: currentDetail.id,
              permission,
              decision,
            });
            toast(`${permission}: ${decision}`, "success");
            // Re-render this row to reflect the new active state.
            for (const b of row.querySelectorAll("button[data-decision]")) {
              const isActive = b.dataset.decision === decision;
              b.classList.toggle("active", isActive);
              b.setAttribute("aria-checked", isActive ? "true" : "false");
            }
            // The set succeeded; the host appended a row to the
            // audit log. Re-fetch it so the user sees their own
            // decision at the top of the table without a manual
            // refresh.
            loadAuditLog(currentDetail.id);
          } catch (error) {
            showError(`set_permission ${permission}`, error);
          }
        });
      }
    }
  }

  async function refreshDetail() {
    if (!currentDetail) return;
    const id = currentDetail.id;
    try {
      const [details, services, runtime] = await Promise.all([
        call("manager.get_app", { id }),
        call("manager.list_services", { id }).catch(() => ({ services: [] })),
        call("manager.runtime_status", { id }).catch(() => null),
      ]);
      currentDetail = details;
      currentServices = Array.isArray(services?.services)
        ? services.services
        : [];
      renderDetail(details, runtime);
      loadAuditLog(id);
    } catch (error) {
      showError(`Failed to refresh ${id}`, error);
    }
  }

  // -- Audit log -----------------------------------------------------------
  // The audit log is loaded as part of the detail view and
  // re-fetched after every `set_permission` so the user sees
  // their own grant appear in the table without a manual
  // refresh. A failure to load does not block the rest of the
  // detail view — the audit panel just renders an error note.
  const AUDIT_LIMIT = 50;

  function formatTimestamp(ms) {
    if (!ms) return "—";
    const date = new Date(Number(ms));
    if (isNaN(date.getTime())) return String(ms);
    // Use the user's locale with seconds so two consecutive
    // decisions are still distinguishable in the table.
    return date.toLocaleString();
  }

  async function loadAuditLog(id) {
    const container = $("audit");
    container.innerHTML = `<p class="audit-empty">Loading…</p>`;
    try {
      const result = await call("manager.read_audit_log", {
        id,
        limit: AUDIT_LIMIT,
      });
      const entries = Array.isArray(result?.entries) ? result.entries : [];
      renderAuditLog(entries);
    } catch (error) {
      const code = error?.code ? `[${error.code}] ` : "";
      container.innerHTML = `<p class="audit-error">${escapeText(
        code + (error?.message ?? error),
      )}</p>`;
    }
  }

  function renderAuditLog(entries) {
    const container = $("audit");
    if (!entries.length) {
      container.innerHTML = `<p class="audit-empty">No audit entries yet. Grant or revoke a permission above to populate this view.</p>`;
      return;
    }
    container.innerHTML = entries
      .map((entry) => {
        const decision = entry.decision ?? "prompt";
        return `
          <div class="audit-row" role="listitem">
            <div class="timestamp" title="${escapeText(String(entry.timestampMs ?? ""))}">${escapeText(
          formatTimestamp(entry.timestampMs),
        )}</div>
            <div class="name">${escapeText(entry.permission ?? "—")}</div>
            <div>
              <span class="badge ${stateClass(decision)}">${escapeText(decision)}</span>
            </div>
          </div>`;
      })
      .join("");
  }

  // -- Hash routing ---------------------------------------------------------
  function parseHash() {
    const raw = window.location.hash.replace(/^#/, "");
    if (!raw || raw === "/") return { view: "list" };
    if (raw.startsWith("/app/")) {
      const id = decodeURIComponent(raw.slice("/app/".length));
      return { view: "detail", id };
    }
    return { view: "list" };
  }

  function navigate(hash) {
    if (window.location.hash === hash) {
      onHashChange();
    } else {
      window.location.hash = hash;
    }
  }

  function onHashChange() {
    aiView.hidden = true;
    const route = parseHash();
    if (route.view === "detail" && route.id) {
      loadDetail(route.id);
    } else {
      currentDetail = null;
      detailView.hidden = true;
      listView.hidden = false;
      loadList();
    }
  }

  // -- App-level action wiring ---------------------------------------------
  for (const button of document.querySelectorAll(".detail-actions button")) {
    button.addEventListener("click", async () => {
      if (!currentDetail) return;
      const id = currentDetail.id;
      const action = button.dataset.action;
      try {
        if (action === "launch") {
          await call("manager.launch", { id });
          toast(`${id}: launch requested`, "success");
        } else if (action === "stop") {
          await call("manager.stop", { id });
          toast(`${id}: stop requested`, "success");
        } else if (action === "restart") {
          await call("manager.restart", { id });
          toast(`${id}: restart requested`, "success");
        } else if (action === "uninstall") {
          const ok = await confirmModal(
            `Uninstall ${id}? This stops the app, removes its install directory, and (optionally) its data.`,
            "Uninstall",
            true,
          );
          if (!ok) return;
          await call("manager.uninstall", { id, removeData: false });
          toast(`${id}: uninstalled`, "success");
          navigate("#/");
          await loadList();
          return;
        }
        await refreshDetail();
        await loadList();
      } catch (error) {
        showError(`${id} ${action}`, error);
      }
    });
  }

  backBtn.addEventListener("click", () => navigate("#/"));

  $("apps-view-btn").addEventListener("click", () => navigate("#/"));
  $("ai-view-btn").addEventListener("click", showAiView);
  $("ai-refresh-btn").addEventListener("click", loadAiOverview);
  aiView.addEventListener("click", async (event) => {
    const button = event.target.closest("[data-ai-action]");
    if (!button) return;
    const operation = button.dataset.aiAction;
    const params = {
      operation,
      providerId: button.dataset.providerId,
      appId: button.dataset.appId,
      binding: button.dataset.binding,
      runId: button.dataset.runId,
    };
    if ((operation === "provider.remove" || operation === "mcp.disconnect" || operation === "agent.cancel")
        && !await confirmModal(`Confirm ${operation}?`, "Confirm", true)) return;
    button.disabled = true;
    try {
      await call("manager.ai_action", params);
      toast(`${operation} completed`, "success");
      await loadAiOverview();
    } catch (error) {
      showError(operation, error);
    } finally {
      button.disabled = false;
    }
  });

  searchInput.addEventListener("input", (event) => {
    searchQuery = event.target.value;
    renderList();
  });

  installBtn.addEventListener("click", () => installFile.click());
  installFile.addEventListener("change", async () => {
    const file = installFile.files?.[0];
    installFile.value = "";
    if (!file) return;
    // WebView2 exposes `File.path` for files selected via the system
    // file picker. A missing path means the user picked a virtual /
    // sandboxed file we cannot install.
    if (!file.path) {
      showError("Install", new Error(
        "could not resolve local path for the selected file",
      ));
      return;
    }
    const requireSignature = window.confirm(
      "Require signature?\n\nOK = require valid signature.\nCancel = install any package (DEV MODE).",
    );
    try {
      const result = await call("manager.install", {
        packagePath: file.path,
        requireSignature,
      });
      toast(
        `Installed ${result?.id ?? file.name}`,
        "success",
      );
      await loadList();
    } catch (error) {
      showError("Install", error);
    }
  });

  window.addEventListener("hashchange", onHashChange);
  onHashChange();
})();
