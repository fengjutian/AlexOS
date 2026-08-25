import { useCallback, useEffect, useMemo, useState } from "react";
import { alex } from "@alex/sdk";

type Action = { label: string; run: () => Promise<unknown> };
type ChildWindow = { id: string };
type EventEntry = { at: string; name: string; payload: unknown };

const stringify = (value: unknown) => JSON.stringify(value, null, 2);

export function App() {
  const [status, setStatus] = useState("正在连接 Host…");
  const [message, setMessage] = useState("Hello from Alex Runtime");
  const [output, setOutput] = useState<unknown>({ waiting: true });
  const [events, setEvents] = useState<EventEntry[]>([]);
  const [watchId, setWatchId] = useState<string>();
  const [childWindow, setChildWindow] = useState<ChildWindow>();
  const [fullscreen, setFullscreen] = useState(false);

  const invoke = useCallback(<T,>(method: string, params: unknown = {}) =>
    alex.invoke<T>(method, params), []);
  const execute = useCallback(async (action: () => Promise<unknown>) => {
    try { setOutput(await action()); }
    catch (error) {
      const detail = error as { message?: string; code?: string };
      setOutput({ error: detail?.message ?? String(error), code: detail?.code });
    }
  }, []);

  useEffect(() => {
    const names = ["filesystem.changed", "fileDrop", "window.focusChanged", "window.resized", "window.moved"];
    const dispose = names.map((name) => alex.events.on(name, (payload) => {
      setEvents((items) => [{ at: new Date().toLocaleTimeString(), name, payload }, ...items].slice(0, 80));
    }));
    invoke("system.capabilities").then((caps) => {
      setStatus("Host 已连接"); setOutput(caps);
    }).catch((error) => setStatus(`连接失败：${error instanceof Error ? error.message : String(error)}`));
    return () => dispose.forEach((off) => off());
  }, [invoke]);

  const groups = useMemo<Array<{ title: string; description: string; actions: Action[] }>>(() => [
    { title: "系统与路径", description: "Runtime 身份、能力发现和应用隔离目录", actions: [
      { label: "系统信息", run: () => invoke("system.info") },
      { label: "Capabilities", run: () => invoke("system.capabilities") },
      { label: "应用路径", run: async () => ({ data: await invoke("paths.dataDir"), cache: await invoke("paths.cacheDir"), temp: await invoke("paths.tempDir") }) },
    ] },
    { title: "Storage", description: "应用级键值存储", actions: [
      { label: "保存", run: async () => { await invoke("storage.set", { key: "message", value: message }); return invoke("storage.get", { key: "message" }); } },
      { label: "读取", run: () => invoke("storage.get", { key: "message" }) },
      { label: "键列表", run: () => invoke("storage.keys") },
      { label: "删除", run: () => invoke("storage.delete", { key: "message" }) },
      { label: "清空", run: () => invoke("storage.clear") },
    ] },
    { title: "文件系统", description: "只允许访问 Manifest 授权的 data 目录", actions: [
      { label: "创建 data", run: () => invoke("filesystem.createDir", { path: "data", recursive: true }) },
      { label: "写文件", run: () => invoke("filesystem.writeText", { path: "data/demo.txt", content: message }) },
      { label: "读文件", run: () => invoke("filesystem.readText", { path: "data/demo.txt" }) },
      { label: "是否存在", run: () => invoke("filesystem.exists", { path: "data/demo.txt" }) },
      { label: "文件信息", run: () => invoke("filesystem.stat", { path: "data/demo.txt" }) },
      { label: "目录列表", run: () => invoke("filesystem.readDir", { path: "data" }) },
      { label: "复制", run: () => invoke("filesystem.copy", { from: "data/demo.txt", to: "data/demo-copy.txt" }) },
      { label: "重命名", run: () => invoke("filesystem.rename", { from: "data/demo-copy.txt", to: "data/demo-renamed.txt" }) },
      { label: "删除", run: () => invoke("filesystem.remove", { path: "data/demo-renamed.txt" }) },
      { label: "监听", run: async () => { const result = await invoke<{ subscriptionId: string }>("filesystem.watch", { path: "data" }); setWatchId(result.subscriptionId); return result; } },
      { label: "取消监听", run: () => watchId ? invoke("filesystem.unwatch", { subscriptionId: watchId }) : Promise.resolve({ skipped: "请先开始监听" }) },
    ] },
    { title: "对话框与桌面", description: "原生选择器、剪贴板、通知和外部链接", actions: [
      { label: "选择文件", run: () => invoke("dialog.openFile", { title: "选择一个文件" }) },
      { label: "多选文件", run: () => invoke("dialog.openFiles", { title: "选择多个文件" }) },
      { label: "选择目录", run: () => invoke("dialog.openDirectory", { title: "选择目录" }) },
      { label: "保存文件", run: () => invoke("dialog.saveFile", { title: "保存示例文件", suggestedName: "alex-demo.txt" }) },
      { label: "复制文本", run: () => invoke("clipboard.writeText", { text: message }) },
      { label: "读剪贴板", run: () => invoke("clipboard.readText") },
      { label: "发送通知", run: () => invoke("notification.show", { title: "Alex Runtime", body: message }) },
      { label: "打开网页", run: () => invoke("system.openExternal", { url: "https://example.com" }) },
    ] },
    { title: "窗口", description: "创建和控制 Runtime 原生窗口", actions: [
      { label: "修改标题", run: () => invoke("window.setTitle", { title: message }) },
      { label: "创建子窗口", run: async () => { const child = await invoke<ChildWindow>("window.create", { url: "index.html", title: "Desktop API 子窗口", width: 640, height: 520 }); setChildWindow(child); return child; } },
      { label: "窗口列表", run: () => invoke("window.list") },
      { label: "调整子窗口", run: () => childWindow ? invoke("window.setBounds", { windowId: childWindow.id, x: 120, y: 120, width: 760, height: 560 }) : Promise.resolve({ skipped: "请先创建子窗口" }) },
      { label: "切换全屏", run: async () => { if (!childWindow) return { skipped: "请先创建子窗口" }; const next = !fullscreen; setFullscreen(next); return invoke("window.setFullscreen", { windowId: childWindow.id, fullscreen: next }); } },
      { label: "关闭子窗口", run: async () => { if (!childWindow) return { skipped: "请先创建子窗口" }; const result = await invoke("window.destroy", { windowId: childWindow.id }); setChildWindow(undefined); return result; } },
    ] },
  ], [childWindow, fullscreen, invoke, message, watchId]);

  return <main>
    <header><div><span className="eyebrow">ALEX RUNTIME · REACT</span><h1>Desktop API Playground</h1><p>在一个标准 React 项目里探索原生桌面能力。</p></div><span className={status.startsWith("连接失败") ? "status error" : "status"}>{status}</span></header>
    <div className="workspace">
      <div className="controls">
        <label>共享测试文本<input value={message} onChange={(event) => setMessage(event.target.value)} /></label>
        {groups.map((group) => <section key={group.title}><div className="section-title"><div><h2>{group.title}</h2><p>{group.description}</p></div><span>{group.actions.length}</span></div><div className="actions">{group.actions.map((action) => <button key={action.label} onClick={() => execute(action.run)}>{action.label}</button>)}</div></section>)}
      </div>
      <aside><div className="panel-heading"><span>调用结果</span><button className="ghost" onClick={() => setOutput({ cleared: true })}>清空</button></div><pre>{stringify(output)}</pre><div className="panel-heading events-title"><span>事件流</span><button className="ghost" onClick={() => setEvents([])}>清空</button></div><div className="events">{events.length === 0 ? <p className="empty">等待窗口、文件或拖放事件…</p> : events.map((entry, index) => <article key={`${entry.at}-${index}`}><b>{entry.name}</b><time>{entry.at}</time><pre>{stringify(entry.payload)}</pre></article>)}</div></aside>
    </div>
  </main>;
}
