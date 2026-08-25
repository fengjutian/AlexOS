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
    item({
      id: "file.openFile",
      label: "打开文件…",
      accelerator: "CmdOrCtrl+O",
      run: () => desktop.dialog.openFile("选择一个文件"),
    }),
    item({
      id: "file.openDirectory",
      label: "打开目录…",
      run: () => desktop.dialog.openDirectory("选择目录"),
    }),
    sep("file.sep1"),
    item({
      id: "file.saveStorage",
      label: "保存到存储",
      accelerator: "CmdOrCtrl+S",
      run: () => desktop.storage.set("message", input.message),
    }),
    item({
      id: "file.saveAs",
      label: "另存为…",
      accelerator: "CmdOrCtrl+Shift+S",
      run: () => desktop.dialog.saveFile("保存示例文件", "alex-demo.txt"),
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
    item({
      id: "edit.storageKeys",
      label: "列出全部 key",
      run: () => desktop.storage.keys(),
    }),
    item({
      id: "edit.clearStorage",
      label: "清空存储",
      run: () => desktop.storage.clear(),
    }),
  ];

  // ---------- view ----------
  const viewItems: MenuItemSpec[] = [
    item({
      id: "view.windowList",
      label: "窗口列表",
      run: () => desktop.window.list(),
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
    item({
      id: "view.closeWindow",
      label: "关闭当前窗口",
      accelerator: "CmdOrCtrl+W",
      run: () => desktop.window.close(currentWindowId()),
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
    item({
      id: "run.toggleWatch",
      label: input.watchId ? "停止监听 data/" : "开始监听 data/",
      run: async () => {
        if (input.watchId) {
          const id = input.watchId;
          input.setWatchId(null);
          return desktop.fs.unwatch(id);
        }
        const result = await desktop.fs.watch("data");
        input.setWatchId(result.subscriptionId);
        return result;
      },
    }),
    item({
      id: "run.toggleShortcut",
      label: "切换 CmdOrCtrl+Shift+D",
      run: async () => {
        const list = await desktop.shortcuts.list();
        const already = list.shortcuts.includes("CmdOrCtrl+Shift+D");
        return already
          ? desktop.shortcuts.unregister("CmdOrCtrl+Shift+D")
          : desktop.shortcuts.register("CmdOrCtrl+Shift+D");
      },
    }),
  ];

  // ---------- help ----------
  const helpItems: MenuItemSpec[] = [
    item({
      id: "help.systemInfo",
      label: "系统信息",
      run: () => desktop.system.info(),
    }),
    item({
      id: "help.capabilities",
      label: "能力列表",
      run: () => desktop.system.capabilities(),
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

/** Walk the menu tree and register every normal item under its full id path. */
function registerMenu(
  menu: MenuSpec,
  actions: MenuActionLookup,
  register: (id: string, label: string, fn: () => Promise<unknown>) => void,
): void {
  const walk = (items: MenuItemSpec[], prefix: string): void => {
    for (const entry of items) {
      if (entry.type === "separator" || !entry.run) continue;
      const id = `${prefix}${entry.id}`;
      register(id, entry.label, () => Promise.resolve(entry.run!()));
    }
  };
  walk(menu.items, `${menu.id}.`);
}
