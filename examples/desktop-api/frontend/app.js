const $ = (selector) => document.querySelector(selector);
const output = $("#output");
const events = $("#events");
const status = $("#status");
const invoke = (method, params = {}) => window.alex.invoke(method, params);
let watchId;
let childWindow;
let fullscreen = false;

function show(value) { output.textContent = JSON.stringify(value, null, 2); }
function log(name, value) { events.textContent = `${new Date().toLocaleTimeString()} ${name}\n${JSON.stringify(value, null, 2)}\n\n${events.textContent}`; }
async function run(task) { try { show(await task()); } catch (error) { show({ error: error?.message ?? error, code: error?.code }); } }

const actions = {
  info: () => invoke("system.info"),
  caps: () => invoke("system.capabilities"),
  paths: async () => ({ data: await invoke("paths.dataDir"), cache: await invoke("paths.cacheDir"), temp: await invoke("paths.tempDir") }),
  store: async () => { await invoke("storage.set", { key: "message", value: $("#text").value }); return invoke("storage.get", { key: "message" }); },
  getStore: () => invoke("storage.get", { key: "message" }),
  keys: () => invoke("storage.keys"),
  deleteStore: () => invoke("storage.delete", { key: "message" }),
  clearStore: () => invoke("storage.clear"),
  mkdir: () => invoke("filesystem.createDir", { path: "data", recursive: true }),
  write: () => invoke("filesystem.writeText", { path: "data/demo.txt", content: $("#text").value }),
  read: () => invoke("filesystem.readText", { path: "data/demo.txt" }),
  exists: () => invoke("filesystem.exists", { path: "data/demo.txt" }),
  stat: () => invoke("filesystem.stat", { path: "data/demo.txt" }),
  dir: () => invoke("filesystem.readDir", { path: "data" }),
  copyFile: () => invoke("filesystem.copy", { from: "data/demo.txt", to: "data/demo-copy.txt" }),
  renameFile: () => invoke("filesystem.rename", { from: "data/demo-copy.txt", to: "data/demo-renamed.txt" }),
  removeFile: () => invoke("filesystem.remove", { path: "data/demo-renamed.txt" }),
  watch: async () => { const result = await invoke("filesystem.watch", { path: "data" }); watchId = result.subscriptionId; return result; },
  unwatch: () => watchId ? invoke("filesystem.unwatch", { subscriptionId: watchId }) : Promise.resolve({ skipped: "请先开始监听" }),
  open: () => invoke("dialog.openFile", { title: "选择一个文件" }),
  openMany: () => invoke("dialog.openFiles", { title: "选择多个文件" }),
  openDir: () => invoke("dialog.openDirectory", { title: "选择目录" }),
  save: () => invoke("dialog.saveFile", { title: "保存示例文件", suggestedName: "alex-demo.txt" }),
  copy: () => invoke("clipboard.writeText", { text: $("#text").value }),
  paste: () => invoke("clipboard.readText"),
  notify: () => invoke("notification.show", { title: "Alex OS", body: $("#text").value }),
  title: () => invoke("window.setTitle", { title: $("#text").value }),
  child: async () => { childWindow = await invoke("window.create", { url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 }); return childWindow; },
  windows: () => invoke("window.list"),
  bounds: () => childWindow ? invoke("window.setBounds", { windowId: childWindow.id, x: 120, y: 120, width: 760, height: 560 }) : Promise.resolve({ skipped: "请先创建子窗口" }),
  fullscreen: async () => { if (!childWindow) return { skipped: "请先创建子窗口" }; fullscreen = !fullscreen; return invoke("window.setFullscreen", { windowId: childWindow.id, fullscreen }); },
  destroy: async () => { if (!childWindow) return { skipped: "请先创建子窗口" }; const result = await invoke("window.destroy", { windowId: childWindow.id }); childWindow = undefined; return result; },
  external: () => invoke("system.openExternal", { url: "https://example.com" }),
  clearEvents: () => { events.textContent = ""; return Promise.resolve({ cleared: true }); },
};

document.addEventListener("click", (event) => { const action = event.target?.dataset?.action; if (actions[action]) run(actions[action]); });
for (const name of ["filesystem.changed", "fileDrop", "window.focusChanged", "window.resized", "window.moved"]) window.alex.on(name, (payload) => log(name, payload));

(async () => { try { const caps = await invoke("system.capabilities"); status.textContent = "Host 已连接"; show(caps); } catch (error) { status.textContent = `连接失败：${error?.message ?? error}`; status.className = "error"; } })();
