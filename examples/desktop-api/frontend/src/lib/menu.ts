/**
 * VSCode-style top menu definition.
 *
 * The structure is the single source of truth for **two** renderings:
 *  1. The in-page `<MenuBar />` at the top of the React app, which
 *     calls `run` directly on click.
 *  2. The host's native menu bar, populated by `menu.setApplicationMenu`
 *     on mount. The native bar emits `menu.clicked` events with the
 *     item id; `App.tsx` looks the id up in `actions` and routes it
 *     back to the same handler.
 *
 * Putting both renderings behind one builder means an item added here
 * automatically appears in both places and the click-to-action mapping
 * stays consistent.
 */
import { desktop } from "./desktop.js";
import type { MenuItemSpec, MenuSpec } from "../types/desktop.js";

/** Menu id → handler. The `menu.clicked` listener walks this map. */
export type MenuActionLookup = Map<string, (label: string) => void>;

export interface BuildMenuInput {
  message: string;
  watchId: string | null;
  childWindowId: number | null;
  /** Runner from `useActionRunner` — drives the result panel. */
  run: (label: string, fn: () => Promise<unknown>) => Promise<void>;
  setWatchId: (next: string | null) => void;
  setChildWindowId: (next: number | null) => void;
}

/** Result of building the menus. `actions[id]` is the handler for a `menu.clicked` event. */
export interface BuiltMenu {
  menus: MenuSpec[];
  actions: MenuActionLookup;
}

/** Resolve the window id of the *current* window for menu actions. */
function currentWindowId(): number {
  const raw = new URLSearchParams(window.location.search).get("window");
  if (raw) {
    const n = Number(raw);
    if (Number.isFinite(n)) return n;
  }
  // 0 = host-defined "current window" sentinel used by minimize / close.
  return 0;
}

export function buildMenus(input: BuildMenuInput): BuiltMenu {
  const actions: MenuActionLookup = new Map();
  const register = (id: string, label: string, fn: () => Promise<unknown>) => {
    actions.set(id, () => {
      void input.run(label, fn);
    });
  };

  // ---------- file ----------
  const fileItems: MenuItemSpec[] = [
    item({
      id: "file.newWindow",
      label: "新建窗口",
      accelerator: "CmdOrCtrl+N",
      run: () =>
        desktop.window
          .create({ url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 })
          .then((info) => {
            input.setChildWindowId(info.id);
            return info;
          }),
    }),
    // Submenu — opens to the right with the file / directory pickers.
    item({
      id: "file.open",
      label: "打开",
      accelerator: "CmdOrCtrl+O",
      items: [
        item({ id: "file.open.file", label: "文件…", run: () => desktop.dialog.openFile("选择一个文件") }),
        item({ id: "file.open.dir", label: "目录…", run: () => desktop.dialog.openDirectory("选择目录") }),
      ],
    }),
    item({
      id: "file.saveAs",
      label: "另存为…",
      accelerator: "CmdOrCtrl+Shift+S",
      run: () => desktop.dialog.saveFile("保存示例文件", "alex-demo.txt"),
    }),
    sep("file.sep1"),
    item({
      id: "file.saveStorage",
      label: "保存到存储",
      accelerator: "CmdOrCtrl+S",
      run: () => desktop.storage.set("message", input.message),
    }),
    sep("file.sep2"),
    item({
      id: "file.exit",
      label: "退出",
      accelerator: "CmdOrCtrl+Q",
      run: () => desktop.window.close(currentWindowId()),
    }),
  ];

  // ---------- edit ----------
  const editItems: MenuItemSpec[] = [
    item({
      id: "edit.copy",
      label: "复制共享文本",
      accelerator: "CmdOrCtrl+C",
      run: () => desktop.clipboard.writeText(input.message),
    }),
    item({
      id: "edit.paste",
      label: "粘贴剪贴板",
      accelerator: "CmdOrCtrl+V",
      run: () => desktop.clipboard.readText(),
    }),
    sep("edit.sep1"),
    // Submenu — storage CRUD grouped under one entry.
    item({
      id: "edit.storage",
      label: "存储",
      items: [
        item({ id: "edit.storage.save", label: "保存 message", run: () => desktop.storage.set("message", input.message) }),
        item({ id: "edit.storage.read", label: "读取 message", run: () => desktop.storage.get("message") }),
        item({ id: "edit.storage.keys", label: "列出全部 key", run: () => desktop.storage.keys() }),
        item({ id: "edit.storage.clear", label: "清空", run: () => desktop.storage.clear() }),
      ],
    }),
  ];

  // ---------- view ----------
  const viewItems: MenuItemSpec[] = [
    // Submenu — window CRUD grouped together.
    item({
      id: "view.windows",
      label: "窗口",
      items: [
        item({ id: "view.windows.list", label: "窗口列表", run: () => desktop.window.list() }),
        item({ id: "view.windows.new", label: "新建窗口", run: () => desktop.window
          .create({ url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 })
          .then((info) => { input.setChildWindowId(info.id); return info; }) }),
        item({ id: "view.windows.close", label: "关闭当前", accelerator: "CmdOrCtrl+W", run: () => desktop.window.close(currentWindowId()) }),
      ],
    }),
    item({
      id: "view.fullscreen",
      label: "切换全屏",
      accelerator: "F11",
      run: () => desktop.window.setFullscreen(currentWindowId(), !document.fullscreenElement),
    }),
    sep("view.sep1"),
    item({
      id: "view.minimize",
      label: "最小化当前窗口",
      run: () => desktop.window.minimize(currentWindowId()),
    }),
    item({
      id: "view.maximize",
      label: "最大化当前窗口",
      run: () => desktop.window.maximize(currentWindowId()),
    }),
  ];

  // ---------- run ----------
  const runItems: MenuItemSpec[] = [
    item({
      id: "run.notify",
      label: "发送通知",
      run: () => desktop.notification.show("Alex Runtime", input.message),
    }),
    item({
      id: "run.openExternal",
      label: "打开 example.com",
      run: () => desktop.system.openExternal("https://example.com"),
    }),
    sep("run.sep1"),
    // Submenu — explicit start/stop instead of a single toggle, so the
    // user can see which watch ids are live without checking state.
    item({
      id: "run.watch",
      label: "文件监听",
      items: [
        item({
          id: "run.watch.start",
          label: input.watchId ? "重新开始 data/" : "开始 data/",
          run: async () => {
            if (input.watchId) {
              const old = input.watchId;
              input.setWatchId(null);
              await desktop.fs.unwatch(old);
            }
            const result = await desktop.fs.watch("data");
            input.setWatchId(result.subscriptionId);
            return result;
          },
        }),
        item({
          id: "run.watch.stop",
          label: "停止",
          run: input.watchId
            ? () => {
                const id = input.watchId;
                if (!id) return Promise.resolve({ skipped: "尚未开始监听" });
                input.setWatchId(null);
                return desktop.fs.unwatch(id);
              }
            : () => Promise.resolve({ skipped: "尚未开始监听" }),
        }),
      ],
    }),
    // Submenu — explicit register/unregister for the demo shortcut.
    item({
      id: "run.shortcut",
      label: "快捷键",
      items: [
        item({ id: "run.shortcut.register", label: "注册 CmdOrCtrl+Shift+D", run: () => desktop.shortcuts.register("CmdOrCtrl+Shift+D") }),
        item({ id: "run.shortcut.unregister", label: "注销", run: () => desktop.shortcuts.unregister("CmdOrCtrl+Shift+D") }),
      ],
    }),
  ];

  // ---------- help ----------
  const helpItems: MenuItemSpec[] = [
    // Submenu — host info grouped under one entry.
    item({
      id: "help.info",
      label: "信息",
      items: [
        item({ id: "help.info.system", label: "系统信息", run: () => desktop.system.info() }),
        item({ id: "help.info.capabilities", label: "能力列表", run: () => desktop.system.capabilities() }),
      ],
    }),
    sep("help.sep1"),
    item({
      id: "help.about",
      label: "关于",
      run: async () => ({
        app: "Desktop API Demo",
        version: "0.1.0",
        sdk: "@alex/sdk",
        os: (await desktop.system.info()).os,
      }),
    }),
  ];

  const menus: MenuSpec[] = [
    { id: "file", label: "文件", items: fileItems },
    { id: "edit", label: "编辑", items: editItems },
    { id: "view", label: "视图", items: viewItems },
    { id: "run", label: "运行", items: runItems },
    { id: "help", label: "帮助", items: helpItems },
  ];

  // Populate the lookup table the `menu.clicked` listener walks.
  for (const menu of menus) registerMenu(menu, actions, register);

  return { menus, actions };
}

// ---- helpers ---------------------------------------------------------------

function item(spec: Omit<MenuItemSpec, "type">): MenuItemSpec {
  return { type: "normal", ...spec };
}

function sep(id: string): MenuItemSpec {
  return { type: "separator", id, label: "" };
}

/**
 * Walk the menu tree and register every *normal* item (i.e. one that
 * carries a `run` callback) under its full id path. Submenu parents
 * are skipped — only their leaf children become clickable.
 */
function registerMenu(
  menu: MenuSpec,
  actions: MenuActionLookup,
  register: (id: string, label: string, fn: () => Promise<unknown>) => void,
): void {
  const walk = (items: MenuItemSpec[], prefix: string): void => {
    for (const entry of items) {
      if (entry.type === "separator") continue;
      const fullId = `${prefix}${entry.id}`;
      if (entry.items && entry.items.length > 0) {
        walk(entry.items, `${fullId}.`);
        continue;
      }
      if (!entry.run) continue;
      register(fullId, entry.label, () => Promise.resolve(entry.run!()));
    }
  };
  walk(menu.items, `${menu.id}.`);
}
