/**
 * Top-level component. Wires together the host handshake, the event
 * stream, the action runner, and the action groups. Each group is a
 * pure data structure (`ActionGroupSpec`) so adding or reordering
 * examples is a single-line edit.
 */
import { useMemo, useState } from "react";
import type React from "react";
import { ActionGroup } from "./components/ActionGroup.js";
import { AppHeader } from "./components/AppHeader.js";
import { EventStream } from "./components/EventStream.js";
import { ResultPanel } from "./components/ResultPanel.js";
import { SharedInput } from "./components/SharedInput.js";
import { useActionRunner } from "./hooks/useActionRunner.js";
import { useEventStream } from "./hooks/useEventStream.js";
import { useHostStatus } from "./hooks/useHostStatus.js";
import { desktop } from "./lib/desktop.js";
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

  // Build the action groups on every render. `useMemo` would only buy
  // us a re-render skip on `run` reference change, but the groups also
  // depend on local state (watchId, childWindowId, message) — easier
  // to keep the dependency list explicit at the call site.
  const groups = useActionGroups({ message, watchId, childWindowId, setWatchId, setChildWindowId, run });

  return (
    <main>
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
          {groups.map((group) => (
            <ActionGroup key={group.title} group={group} pending={state.pending} onRun={run} />
          ))}
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
  setWatchId: (next: string | null) => void;
  setChildWindowId: (next: number | null) => void;
  run: (label: string, fn: () => Promise<unknown>) => Promise<void>;
}

/**
 * Build the ordered list of action groups shown in the left column.
 * Splitting the data out of the component keeps App.tsx focused on
 * composition and makes the demo surface a one-screen summary of
 * every host capability exercised.
 */
function useActionGroups(input: UseActionGroupsInput): ActionGroupSpec[] {
  const { message, watchId, childWindowId, setWatchId, setChildWindowId, run } = input;

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

    const menuAndTray: ActionGroupSpec = {
      title: "应用菜单与托盘",
      description: "注册原生菜单模板和系统托盘图标。",
      actions: [
        {
          label: "注册应用菜单",
          description: "menu.setApplicationMenu — 用普通/分隔/复选三种类型拼一份菜单。",
          run: () =>
            desktop.menu.setApplicationMenu({
              items: [
                { type: "normal", id: "demo.reload", label: "重载视图", accelerator: "CmdOrCtrl+R" },
                { type: "normal", id: "demo.beep", label: "蜂鸣" },
                { type: "separator" },
                {
                  type: "submenu",
                  id: "demo.submenu",
                  label: "子菜单",
                  items: [
                    { type: "checkbox", id: "demo.flag", label: "启用日志", checked: true },
                    { type: "normal", id: "demo.about", label: "关于…" },
                  ],
                },
              ],
            }),
        },
        {
          label: "创建托盘",
          description: "tray.create — 创建一个 1x1 透明托盘图标；返回 id 可被 destroy。",
          run: async () => {
            const result = await desktop.tray.create({ icon: "data:image/png;base64,", tooltip: "Desktop API Demo" });
            return result;
          },
        },
        {
          label: "销毁托盘",
          description: "tray.destroy — 关闭最近一次创建（demo 不会替你记录 id，先 create 再 destroy）。",
          run: async () => {
            // The host returns the id at create time; the demo shows
            // a single-shot id via the most recent event payload if
            // you want to chain manually. Here we fail loudly if no
            // id is known.
            throw new Error("请先在结果面板里拿到 tray.create 返回的 id，然后通过 console 调用 tray.destroy(id)");
          },
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
      title: "通知与外部链接",
      description: "系统通知、openExternal 走 host allow-list，源不在白名单会拒绝。",
      actions: [
        { label: "发送通知", description: "notification.show — title/body 都用共享文本。", run: () => desktop.notification.show("Alex Runtime", message) },
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

    return [systemAndPaths, menuAndTray, shortcuts, storage, filesystem, dialogs, notificationAndLinks, windows];
    // `run` comes from a stable callback in the parent, so listing it
    // here is safe.
  }, [message, watchId, childWindowId, run, setWatchId, setChildWindowId]);
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

// Silence the unused-symbol lint for the type when the file gets
// reused; the import is also helpful for editors.
export type { ActionSpec };
