import { useEffect, useState } from "react";

declare global {
  interface Window {
    alex: {
      invoke<T = unknown>(method: string, params?: unknown): Promise<T>;
      on(event: string, listener: (data: unknown) => void): () => void;
    };
  }
}

type Note = { id: number; title: string; body: string; at: number };

// On Windows WebView2, `fetch` rejects the `alex://` custom scheme.
// Wry rewrites the navigation URL to `http://alex.app/...` before
// the host handler sees it, so the page must use the rewritten
// form to reach the alex://app/api/* reverse proxy that forwards
// to 127.0.0.1:ALEX_SERVICE_PORT with the host-minted token.
const api = <T = unknown>(path: string, init?: RequestInit): Promise<T> =>
  fetch(`http://alex.app${path}`, init).then(async (r) => {
    const text = await r.text();
    if (!r.ok) throw new Error(`HTTP ${r.status}: ${text}`);
    return (text ? JSON.parse(text) : null) as T;
  });

export function App() {
  const [now, setNow] = useState(() => new Date().toISOString());
  const [serverTime, setServerTime] = useState<string>("—");
  const [info, setInfo] = useState<string>("—");
  const [saved, setSaved] = useState<string>("(empty)");
  const [notes, setNotes] = useState<Note[]>([]);
  const [newTitle, setNewTitle] = useState("");
  const [newBody, setNewBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const t = window.setInterval(() => setNow(new Date().toISOString()), 1000);
    return () => window.clearInterval(t);
  }, []);

  useEffect(() => {
    window.alex
      .invoke<{ name: string; version: string }>("system.info")
      .then((r) => setInfo(`${r.name} v${r.version}`))
      .catch((e) => console.warn("system.info:", e));

    window.alex
      .invoke<{ value: string | null }>("storage.get", { key: "greeting" })
      .then((r) => r.value && setSaved(r.value))
      .catch(() => {});

    const off = window.alex.on("permission.changed", (data) =>
      console.log("permission changed:", data),
    );
    return off;
  }, []);

  const refreshServerTime = () =>
    api<{ now: string }>("/api/time")
      .then((r) => setServerTime(r.now))
      .catch((e) => setError(String(e)));

  const refreshNotes = () =>
    api<{ items: Note[] }>("/api/notes")
      .then((r) => setNotes(r.items))
      .catch((e) => setError(String(e)));

  const addNote = async () => {
    const title = newTitle.trim();
    if (!title) return;
    setBusy(true);
    setError(null);
    try {
      await api("/api/notes", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title, body: newBody }),
      });
      setNewTitle("");
      setNewBody("");
      await refreshNotes();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const deleteNote = async (id: number) => {
    setBusy(true);
    try {
      await api(`/api/notes/${id}`, { method: "DELETE" });
      await refreshNotes();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: 24, maxWidth: 720 }}>
      <h1>Alex OS · React + Express</h1>
      <p>
        App: <strong>{info}</strong> · client time: <code>{now}</code>
      </p>
      <p>
        server time: <code>{serverTime}</code> · storage["greeting"]: <code>{saved}</code>
      </p>
      {error && (
        <p style={{ color: "crimson" }}>error: {error}</p>
      )}

      <section style={{ marginTop: 16 }}>
        <h2>Backend (Express service mode)</h2>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button onClick={refreshServerTime} disabled={busy}>
            GET /api/time
          </button>
          <button onClick={refreshNotes} disabled={busy}>
            GET /api/notes
          </button>
        </div>
        <ul style={{ marginTop: 12 }}>
          {notes.length === 0 && <li>(no notes)</li>}
          {notes.map((n) => (
            <li key={n.id} style={{ marginBottom: 4 }}>
              <strong>#{n.id}</strong> {n.title}
              {n.body ? ` — ${n.body}` : ""}{" "}
              <button onClick={() => deleteNote(n.id)} disabled={busy} style={{ marginLeft: 8 }}>
                delete
              </button>
            </li>
          ))}
        </ul>
        <div style={{ display: "flex", flexDirection: "column", gap: 4, maxWidth: 360 }}>
          <input
            placeholder="title"
            value={newTitle}
            onChange={(e) => setNewTitle(e.target.value)}
          />
          <input
            placeholder="body (optional)"
            value={newBody}
            onChange={(e) => setNewBody(e.target.value)}
          />
          <button onClick={addNote} disabled={busy || !newTitle.trim()}>
            POST /api/notes
          </button>
        </div>
      </section>

      <section style={{ marginTop: 24 }}>
        <h2>Host API (window.alex.invoke)</h2>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            onClick={() =>
              window.alex.invoke("clipboard.writeText", { text: "copied from React" })
            }
          >
            clipboard.writeText
          </button>
          <button onClick={() => window.alex.invoke("window.setTitle", { title: "Title from React" })}>
            window.setTitle
          </button>
          <button
            onClick={() => {
              const value = `hello @ ${new Date().toLocaleTimeString()}`;
              window.alex.invoke("storage.set", { key: "greeting", value }).then(() => setSaved(value));
            }}
          >
            storage.set
          </button>
        </div>
      </section>
    </main>
  );
}
