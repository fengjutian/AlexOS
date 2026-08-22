import { useEffect, useState } from "react";

declare global {
  interface Window {
    alex: {
      invoke<T = unknown>(method: string, params?: unknown): Promise<T>;
      on(event: string, listener: (data: unknown) => void): () => void;
    };
  }
}

export function App() {
  const [now, setNow] = useState(() => new Date().toISOString());
  const [info, setInfo] = useState<string>("—");
  const [saved, setSaved] = useState<string>("(empty)");

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNow(new Date().toISOString());
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    // Pull host app metadata once on mount.
    window.alex
      .invoke<{ name: string; version: string }>("system.info")
      .then((r) => setInfo(`${r.name} v${r.version}`))
      .catch((err) => console.warn("system.info failed:", err));

    // Read a previously-saved greeting from the app's persistent store.
    window.alex
      .invoke<{ value: string | null }>("storage.get", { key: "greeting" })
      .then((r) => r.value && setSaved(r.value))
      .catch(() => {});

    // Subscribe to host-pushed events. Returns an unsubscribe function.
    const off = window.alex.on("permission.changed", (data) => {
      console.log("permission changed:", data);
    });
    return off;
  }, []);

  const writeClipboard = () =>
    window.alex.invoke("clipboard.writeText", { text: "copied from React" });

  const setWindowTitle = () =>
    window.alex.invoke("window.setTitle", { title: "Title from React" });

  const persistGreeting = () => {
    const value = `hello @ ${new Date().toLocaleTimeString()}`;
    window.alex.invoke("storage.set", { key: "greeting", value }).then(() => {
      setSaved(value);
    });
  };

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: 24 }}>
      <h1>Alex OS · React + TypeScript</h1>
      <p>App: <strong>{info}</strong></p>
      <p>Current time: <strong>{now}</strong></p>
      <p>storage["greeting"] = <code>{saved}</code></p>
      <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
        <button onClick={writeClipboard}>写剪贴板</button>
        <button onClick={setWindowTitle}>改窗口标题</button>
        <button onClick={persistGreeting}>保存 greeting</button>
      </div>
    </main>
  );
}
