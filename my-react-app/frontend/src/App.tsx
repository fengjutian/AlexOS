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

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNow(new Date().toISOString());
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: 24 }}>
      <h1>Alex OS · React + TypeScript</h1>
      <p>This is a React + TypeScript app running inside the Alex OS WebView.</p>
      <p>
        Use <code>window.alex.invoke("…", {"{…}"})</code> to call host APIs. The
        BRIDGE is injected automatically before this module loads.
      </p>
      <p>Current time: <strong>{now}</strong></p>
    </main>
  );
}
