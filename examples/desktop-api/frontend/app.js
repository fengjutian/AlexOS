const $ = (selector) => document.querySelector(selector);
const output = $("#output");
const events = $("#events");
const status = $("#status");
const invoke = (method, params = {}) => window.alex.invoke(method, params);

function show(value) { output.textContent = JSON.stringify(value, null, 2); }
function log(name, value) { events.textContent = `${new Date().toLocaleTimeString()} ${name}\n${JSON.stringify(value, null, 2)}\n\n${events.textContent}`; }
async function run(task) { try { show(await task()); } catch (error) { show({ error: error?.message ?? error, code: error?.code }); } }

const actions = {
  info: () => invoke("system.info"),
  paths: async () => ({ data: await invoke("paths.dataDir"), cache: await invoke("paths.cacheDir"), temp: await invoke("paths.tempDir") }),
  store: async () => { await invoke("storage.set", { key: "message", value: $("#text").value }); return invoke("storage.get", { key: "message" }); },
  write: () => invoke("filesystem.writeText", { path: "data/demo.txt", content: $("#text").value }),
  read: () => invoke("filesystem.readText", { path: "data/demo.txt" }),
  watch: () => invoke("filesystem.watch", { path: "data" }),
  open: () => invoke("dialog.openFile", { title: "选择一个文件" }),
  save: () => invoke("dialog.saveFile", { title: "保存示例文件", suggestedName: "alex-demo.txt" }),
  copy: () => invoke("clipboard.writeText", { text: $("#text").value }),
  paste: () => invoke("clipboard.readText"),
  notify: () => invoke("notification.show", { title: "Alex OS", body: $("#text").value }),
  title: () => invoke("window.setTitle", { title: $("#text").value }),
  child: () => invoke("window.create", { url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 }),
  external: () => invoke("system.openExternal", { url: "https://example.com" }),
};

document.addEventListener("click", (event) => { const action = event.target?.dataset?.action; if (actions[action]) run(actions[action]); });
for (const name of ["filesystem.changed", "fileDrop", "window.focusChanged", "window.resized", "window.moved"]) window.alex.on(name, (payload) => log(name, payload));

(async () => { try { const caps = await invoke("system.capabilities"); status.textContent = "Host 已连接"; show(caps); } catch (error) { status.textContent = `连接失败：${error?.message ?? error}`; status.className = "error"; } })();
