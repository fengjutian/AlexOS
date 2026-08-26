/**
 * Top-level component. Wires together the host handshake, the event
 * stream, the action runner, the action groups, and the top menu bar.
 * Each group is a pure data structure (`ActionGroupSpec`) so adding
 * or reordering examples is a single-line edit.
 */
import { useEffect, useMemo, useState } from "react";
import type React from "react";
import { alex } from "@alex/sdk";
import { ActionGroup } from "./components/ActionGroup.js";
import { AppHeader } from "./components/AppHeader.js";
import { EventStream } from "./components/EventStream.js";
import { MenuBar } from "./components/MenuBar.js";
import { ResultPanel } from "./components/ResultPanel.js";
import { SharedInput } from "./components/SharedInput.js";
import { useActionRunner } from "./hooks/useActionRunner.js";
import { useEventStream } from "./hooks/useEventStream.js";
import { useHostStatus } from "./hooks/useHostStatus.js";
import { desktop } from "./lib/desktop.js";
import { buildMenus } from "./lib/menu.js";
import type { MenuSpec } from "./types/desktop.js";
import type { ActionGroupSpec, ActionSpec } from "./types/desktop.js";

// The set of host events the demo listens to. Keep this list flat —
// `useEventStream` will resubscribe whenever it changes.
const WATCHED_EVENTS = [
  "filesystem.changed",
  "fileDrop",
  "window.focusChanged",
  "window.resized",
  "window.moved",
  "menu.clicked",
  "shortcut.triggered",
] as const;

// Default payload for several demos: file contents, notification body,
// notification title, … One shared text box drives them all.
const DEFAULT_MESSAGE = "Hello from Alex Runtime";

export function App(): React.ReactElement {
  const { status, capabilities } = useHostStatus();
  const { state, run, clear } = useActionRunner();
  const { events, clear: clearEvents } = useEventStream(WATCHED_EVENTS);
  const [message, setMessage] = useState(DEFAULT_MESSAGE);
  const [watchId, setWatchId] = useState<string | null>(null);
  const [childWindowId, setChildWindowId] = useState<number | null>(null);
  const [trayId, setTrayId] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  // Build the menu tree (used by both the in-page bar and the native
  // menu). Re-built when state the menu reads changes (watchId toggles
  // the label, etc.).
  const { menus, actions: menuActions } = useMemo(
    () => buildMenus({ message, watchId, childWindowId, run, setWatchId, setChildWindowId }),
    [message, watchId, childWindowId, run, setWatchId, setChildWindowId],
  );

  // Register the native menu on first mount and whenever the template
  // changes. Best-effort: if the host denies the menu permission, the
  // in-page bar still works.
  useEffect(() => {
    let cancelled = false;
    void desktop.menu
      .setApplicationMenu(toNativeTemplate(menus))
      .catch((error: unknown) => {
        if (cancelled) return;
        const detail = error as { message?: string };
        // Surface the failure via the result panel so the user knows
        // why the native menu isn't showing.
        void run("注册原生菜单", () =>
          Promise.resolve({ error: detail?.message ?? String(error), note: "in-page 菜单仍可用" }),
        );
      });
    return () => {
      cancelled = true;
    };
  }, [menus, run]);

  // Route native menu clicks (`menu.clicked` events) to the same
  // handlers the in-page bar invokes.
  useEffect(() => {
    const dispose = alex.events.on("menu.clicked", (payload) => {
      const id = (payload as { id?: string })?.id;
      if (!id) return;
      const handler = menuActions.get(id);
      if (handler) handler(deriveLabel(menus, id));
    });
    return dispose;
  }, [menuActions, menus]);

  // Build the action groups on every render. `useMemo` would only buy
  // us a re-render skip on `run` reference change, but the groups also
  // depend on local state (watchId, childWindowId, message) — easier
  // to keep the dependency list explicit at the call site.
  const groups = useActionGroups({
    message,
    watchId,
    childWindowId,
    trayId,
    setWatchId,
    setChildWindowId,
    setTrayId,
    run,
  });
  const filteredGroups = useMemo(() => filterGroups(groups, query), [groups, query]);
  const actionCount = groups.reduce((total, group) => total + group.actions.length, 0);

  return (
    <main>
      <MenuBar menus={menus} onRun={run} />
      <AppHeader status={status} runtime={extractRuntimeId(capabilities)} />
      <div className="workspace">
        <div className="controls">
          <SharedInput
            id="shared-message"
            label="共享测试文本"
            hint="被多个 demo 复用：写文件、通知正文、窗口标题…"
            value={message}
            onChange={setMessage}
          />
          <section className="api-explorer" aria-label="API 浏览器">
            <div>
              <strong>API Explorer</strong>
              <span>{groups.length} 个领域 · {actionCount} 个操作</span>
            </div>
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索 API、功能或方法名…"
              aria-label="搜索 API"
            />
          </section>
          {filteredGroups.map((group) => (
            <ActionGroup key={group.title} group={group} pending={state.pending} onRun={run} />
          ))}
          {filteredGroups.length === 0 && <p className="no-results">没有匹配的 API 操作。</p>}
        </div>
        <div className="results">
          <ResultPanel state={state} onClear={clear} />
          <EventStream events={events} onClear={clearEvents} />
        </div>
      </div>
    </main>
  );
}

// ---------------------------------------------------------------------------
// Group definitions
// ---------------------------------------------------------------------------

interface UseActionGroupsInput {
  message: string;
  watchId: string | null;
  childWindowId: number | null;
  trayId: string | null;
  setWatchId: (next: string | null) => void;
  setChildWindowId: (next: number | null) => void;
  setTrayId: (next: string | null) => void;
  run: (label: string, fn: () => Promise<unknown>) => Promise<void>;
}

/**
 * Build the ordered list of action groups shown in the left column.
 * Splitting the data out of the component keeps App.tsx focused on
 * composition and makes the demo surface a one-screen summary of
 * every host capability exercised.
 */
function useActionGroups(input: UseActionGroupsInput): ActionGroupSpec[] {
  const { message, watchId, childWindowId, trayId, setWatchId, setChildWindowId, setTrayId, run } = input;

  return useMemo<ActionGroupSpec[]>(() => {
    const systemAndPaths: ActionGroupSpec = {
      title: "系统与路径",
      description: "Runtime 身份、能力发现、每应用隔离目录。",
      actions: [
        { label: "系统信息", description: "system.info — OS / arch / Alex 版本 / 协议版本。", run: () => desktop.system.info() },
        { label: "Capabilities", description: "system.capabilities — host 真正开放的能力 + 平台特性。", run: () => desktop.system.capabilities() },
        {
          label: "应用路径",
          description: "paths.{data,cache,temp}Dir — host 为本应用分配的三个隔离目录。",
          run: async () => ({
            data: await desktop.paths.dataDir(),
            cache: await desktop.paths.cacheDir(),
            temp: await desktop.paths.tempDir(),
          }),
        },
      ],
    };

    const tray: ActionGroupSpec = {
      title: "菜单与系统托盘",
      description: "应用菜单、右键菜单和托盘图标的原生生命周期。",
      actions: [
        {
          label: "注册右键菜单",
          description: "menu.setContextMenu — 为 WebView 设置由 Host 渲染的上下文菜单。",
          run: () => desktop.menu.setContextMenu({
            items: [
              { type: "normal", id: "context.copy", label: "复制示例文本" },
              { type: "separator" },
              { type: "normal", id: "context.inspect", label: "查看事件流" },
            ],
          }),
        },
        {
          label: trayId ? "重建托盘" : "创建托盘",
          description: "tray.create — 创建一个透明托盘图标；id 会保存以便销毁。",
          run: async () => {
            const result = await desktop.tray.create({ icon: "data:image/png;base64,", tooltip: "Desktop API Demo" });
            setTrayId(result.id);
            return result;
          },
        },
        {
          label: "销毁托盘",
          description: "tray.destroy — 需要先创建。销毁后 id 清空。",
          run: trayId
            ? async () => {
                const id = trayId;
                setTrayId(null);
                return desktop.tray.destroy(id);
              }
            : () => Promise.resolve({ skipped: "请先创建托盘" }),
        },
      ],
    };

    const shortcuts: ActionGroupSpec = {
      title: "快捷键",
      description: "注册/查询/注销全局热键；事件通过 shortcut.triggered 推回前端。",
      actions: [
        {
          label: "注册 CmdOrCtrl+Shift+D",
          description: "shortcuts.register — 注册后按下会在事件流里看到 shortcut.triggered。",
          run: () => desktop.shortcuts.register("CmdOrCtrl+Shift+D"),
        },
        {
          label: "查询已注册",
          description: "shortcuts.list — 列出本应用注册过的所有 accelerator。",
          run: () => desktop.shortcuts.list(),
        },
        {
          label: "注销 CmdOrCtrl+Shift+D",
          description: "shortcuts.unregister — 与 register 配对使用。",
          run: () => desktop.shortcuts.unregister("CmdOrCtrl+Shift+D"),
        },
      ],
    };

    const storage: ActionGroupSpec = {
      title: "Storage",
      description: "应用级键值存储；键名空间隔离，跨窗口可见。",
      actions: [
        {
          label: "保存 message",
          description: "storage.set — 把当前共享文本以 key=\"message\" 写入。",
          run: () => desktop.storage.set("message", message),
        },
        { label: "读取 message", description: "storage.get — 取回刚才写入的 value。", run: () => desktop.storage.get("message") },
        { label: "键列表", description: "storage.keys — 列出本应用下所有 key。", run: () => desktop.storage.keys() },
        { label: "删除 message", description: "storage.delete — 单 key 删除。", run: () => desktop.storage.delete("message") },
        { label: "清空", description: "storage.clear — 抹掉本应用全部键值。", run: () => desktop.storage.clear() },
      ],
    };

    const filesystem: ActionGroupSpec = {
      title: "文件系统",
      description: "只能访问 manifest 授权的 data 目录，越界会被 host 拒绝。",
      actions: [
        { label: "创建 data/", description: "filesystem.createDir recursive=true — 等价于 mkdir -p。", run: () => desktop.fs.createDir("data", true) },
        { label: "写文件", description: "filesystem.writeText — 写入共享文本到 data/demo.txt。", run: () => desktop.fs.writeText("data/demo.txt", message) },
        { label: "读文件", description: "filesystem.readText — 取回 data/demo.txt 的内容。", run: () => desktop.fs.readText("data/demo.txt") },
        { label: "写二进制", description: "filesystem.writeBinary — 把共享文本编码为 Base64 后写入。", run: () => desktop.fs.writeBinary("data/demo.bin", utf8ToBase64(message)) },
        { label: "读二进制", description: "filesystem.readBinary — 返回 Base64 编码的二进制数据。", run: () => desktop.fs.readBinary("data/demo.bin") },
        { label: "是否存在", description: "filesystem.exists — true/false，不会抛错。", run: () => desktop.fs.exists("data/demo.txt") },
        { label: "文件信息", description: "filesystem.stat — 大小、类型、修改时间。", run: () => desktop.fs.stat("data/demo.txt") },
        { label: "目录列表", description: "filesystem.readDir — 列出 data/ 下所有 entry。", run: () => desktop.fs.readDir("data") },
        { label: "复制", description: "filesystem.copy — demo.txt → demo-copy.txt。", run: () => desktop.fs.copy("data/demo.txt", "data/demo-copy.txt") },
        { label: "重命名", description: "filesystem.rename — copy.txt → renamed.txt。", run: () => desktop.fs.rename("data/demo-copy.txt", "data/demo-renamed.txt") },
        { label: "删除", description: "filesystem.remove — 单文件删除。", run: () => desktop.fs.remove("data/demo-renamed.txt") },
        {
          label: "监听 data/",
          description: "filesystem.watch — 启动 watcher，事件会进右侧事件流。",
          run: async () => {
            const result = await desktop.fs.watch("data");
            setWatchId(result.subscriptionId);
            return result;
          },
        },
        {
          label: "取消监听",
          description: "filesystem.unwatch — 必须先点监听按钮拿到 subscriptionId。",
          run: watchId
            ? () => {
                const id = watchId;
                setWatchId(null);
                return desktop.fs.unwatch(id);
              }
            : () => Promise.resolve({ skipped: "请先点击「监听 data/」" }),
        },
      ],
    };

    const dialogs: ActionGroupSpec = {
      title: "对话框与剪贴板",
      description: "原生选择器、剪贴板读写；token grant 由 host 持有，前端只能拿到 path+token。",
      actions: [
        { label: "选择文件", description: "dialog.openFile — 用户取消返回 null。", run: () => desktop.dialog.openFile("选择一个文件") },
        { label: "多选文件", description: "dialog.openFiles — 始终返回数组。", run: () => desktop.dialog.openFiles("选择多个文件") },
        { label: "选择目录", description: "dialog.openDirectory — 返回 token grant。", run: () => desktop.dialog.openDirectory("选择目录") },
        { label: "保存文件", description: "dialog.saveFile — suggestedName 只是提示。", run: () => desktop.dialog.saveFile("保存示例文件", "alex-demo.txt") },
        { label: "复制到剪贴板", description: "clipboard.writeText — 写入共享文本。", run: () => desktop.clipboard.writeText(message) },
        { label: "读取剪贴板", description: "clipboard.readText — host 会先弹权限框。", run: () => desktop.clipboard.readText() },
      ],
    };

    const notificationAndLinks: ActionGroupSpec = {
      title: "通知、权限与网络",
      description: "系统通知、设备权限、外部链接和受来源白名单保护的 HTTPS 请求。",
      actions: [
        { label: "发送通知", description: "notification.show — title/body 都用共享文本。", run: () => desktop.notification.show("Alex Runtime", message) },
        { label: "请求摄像头权限", description: "system.requestPermission — 授权后才能使用 WebView MediaDevices。", run: () => desktop.system.requestPermission("camera") },
        { label: "请求麦克风权限", description: "system.requestPermission — 权限决定由 Host 和用户共同控制。", run: () => desktop.system.requestPermission("microphone") },
        {
          label: "安全 Fetch",
          description: "net.fetch — HTTPS-only、禁止重定向且限制响应体大小。",
          run: async () => {
            const response = await desktop.net.fetch("https://example.com", { timeoutMs: 10_000, maxBytes: 64_000 });
            return { ...response, bodyPreview: base64ToUtf8(response.body).slice(0, 240), body: `[base64 ${response.body.length} chars]` };
          },
        },
        { label: "打开 example.com", description: "system.openExternal — origin 受 manifest 限制。", run: () => desktop.system.openExternal("https://example.com") },
      ],
    };

    const windows: ActionGroupSpec = {
      title: "窗口",
      description: "原生窗口 CRUD：标题/位置/全屏/最小化/关闭/销毁。",
      actions: [
        { label: "改标题", description: "window.setTitle — 把当前窗口标题改成共享文本。", run: () => desktop.window.setTitle(message) },
        {
          label: "创建子窗口",
          description: "window.create — 打开一个独立 WebView，返回 WindowInfo。",
          run: async () => {
            const info = await desktop.window.create({ url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 });
            setChildWindowId(info.id);
            return info;
          },
        },
        { label: "窗口列表", description: "window.list — 含主窗口与所有子窗口。", run: () => desktop.window.list() },
        {
          label: "主窗口边界",
          description: "window.getBounds — 查询主窗口位置和客户区尺寸。",
          run: async () => desktop.window.getBounds(await firstWindowId()),
        },
        {
          label: "切换主窗口全屏",
          description: "window.isFullscreen + setFullscreen — 读取当前状态后切换。",
          run: async () => {
            const id = await firstWindowId();
            const current = await desktop.window.isFullscreen(id);
            return desktop.window.setFullscreen(id, !current.fullscreen);
          },
        },
        {
          label: "移动子窗口",
          description: "window.setBounds — 需先有子窗口。",
          run: childWindowId === null
            ? () => Promise.resolve({ skipped: "请先创建子窗口" })
            : () => desktop.window.setBounds(childWindowId, { x: 120, y: 120, width: 760, height: 560 }),
        },
        {
          label: "最小化子窗口",
          description: "window.minimize — 需先有子窗口。",
          run: childWindowId === null
            ? () => Promise.resolve({ skipped: "请先创建子窗口" })
            : () => desktop.window.minimize(childWindowId),
        },
        {
          label: "最大化子窗口",
          description: "window.maximize — 需先有子窗口。",
          run: childWindowId === null
            ? () => Promise.resolve({ skipped: "请先创建子窗口" })
            : () => desktop.window.maximize(childWindowId),
        },
        {
          label: "关闭子窗口",
          description: "window.close — 优雅关闭，host 仍会发出 close 事件。",
          run: childWindowId === null
            ? () => Promise.resolve({ skipped: "请先创建子窗口" })
            : async () => {
                const result = await desktop.window.close(childWindowId);
                setChildWindowId(null);
                return result;
              },
        },
        {
          label: "强制销毁",
          description: "window.destroy — 不走 close 流程，常用于异常回收。",
          run: childWindowId === null
            ? () => Promise.resolve({ skipped: "请先创建子窗口" })
            : async () => {
                const result = await desktop.window.destroy(childWindowId);
                setChildWindowId(null);
                return result;
              },
        },
      ],
    };

    return [systemAndPaths, tray, shortcuts, storage, filesystem, dialogs, notificationAndLinks, windows];
    // `run` comes from a stable callback in the parent, so listing it
    // here is safe.
  }, [message, watchId, childWindowId, trayId, run, setWatchId, setChildWindowId, setTrayId]);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/** Pull a runtime id out of the capabilities payload if the host includes one. */
function extractRuntimeId(capabilities: unknown): string | null {
  if (!capabilities || typeof capabilities !== "object") return null;
  const platform = (capabilities as { platform?: { os?: string } }).platform;
  const os = platform?.os ?? "host";
  return `os: ${os}`;
}

function filterGroups(groups: ActionGroupSpec[], query: string): ActionGroupSpec[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return groups;
  return groups
    .map((group) => ({
      ...group,
      actions: group.actions.filter((action) =>
        `${group.title} ${group.description} ${action.label} ${action.description}`
          .toLocaleLowerCase()
          .includes(needle),
      ),
    }))
    .filter((group) => group.actions.length > 0);
}

async function firstWindowId(): Promise<number> {
  const { windows } = await desktop.window.list();
  const first = windows[0];
  if (!first) throw new Error("Host 没有返回可管理窗口");
  return first.id;
}

function utf8ToBase64(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  bytes.forEach((byte) => { binary += String.fromCharCode(byte); });
  return btoa(binary);
}

function base64ToUtf8(value: string): string {
  const binary = atob(value);
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/**
 * Convert the in-page `MenuSpec` tree to the host's `MenuTemplate`
 * shape. We strip the `run` callback (the host only understands id,
 * label, accelerator, type and submenu items) and recurse into nested
 * submenus so the native bar mirrors the in-page one.
 */
function toNativeTemplate(menus: MenuSpec[]) {
  return {
    items: menus.map((menu) => ({
      type: "submenu" as const,
      id: menu.id,
      label: menu.label,
      items: menu.items.map(toNativeItem),
    })),
  };
}

function toNativeItem(
  item: import("./types/desktop.js").MenuItemSpec,
): { type: "normal"; id: string; label: string; accelerator?: string } | { type: "separator" } | { type: "submenu"; id: string; label: string; accelerator?: string; items: ReturnType<typeof toNativeItem>[] } {
  if (item.type === "separator") {
    return { type: "separator" as const };
  }
  // Nested submenu: keep the parent's id (so `menu.clicked` carries
  // the full dotted path back), drop the `run` callback, recurse.
  if (item.items && item.items.length > 0) {
    return {
      type: "submenu" as const,
      id: item.id,
      label: item.label,
      accelerator: item.accelerator,
      items: item.items.map(toNativeItem),
    };
  }
  return {
    type: "normal" as const,
    id: item.id,
    label: item.label,
    accelerator: item.accelerator,
  };
}

/** Walk the menu tree by id and return the matched item's label. */
function deriveLabel(menus: MenuSpec[], fullId: string): string {
  const walk = (items: import("./types/desktop.js").MenuItemSpec[], prefix: string, parents: string[]): string | null => {
    for (const item of items) {
      if (item.type === "separator") continue;
      const id = `${prefix}${item.id}`;
      if (id === fullId) return [...parents, item.label].join(" · ");
      if (item.items && item.items.length > 0) {
        const found = walk(item.items, `${id}.`, [...parents, item.label]);
        if (found) return found;
      }
    }
    return null;
  };
  for (const menu of menus) {
    const found = walk(menu.items, `${menu.id}.`, [menu.label]);
    if (found) return found;
  }
  return fullId;
}

// Silence the unused-symbol lint for the type when the file gets
// reused; the import is also helpful for editors.
export type { ActionSpec };
