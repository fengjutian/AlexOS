// Service-mode Express backend. The host allocates a port from
// 28000-28999 (or honors manifest.backend.port) and injects it
// as ALEX_SERVICE_PORT. The reverse proxy at alex://app/api/*
// adds the host-minted token before forwarding, so this server
// can stay simple and assume the host has already authenticated
// the caller.
const express = require('express');

const port = Number(process.env.ALEX_SERVICE_PORT);
if (!Number.isInteger(port) || port <= 0) {
  process.stderr.write(
    `invalid ALEX_SERVICE_PORT: ${process.env.ALEX_SERVICE_PORT}\n`,
  );
  process.exit(2);
}

const app = express();
app.use(express.json());

app.get('/health', (_req, res) => res.status(200).send('ok'));

// ── meta ───────────────────────────────────────────────────────────
app.get('/api/time', (_req, res) => {
  res.json({ now: new Date().toISOString(), uptime: process.uptime() });
});

app.post('/api/echo', (req, res) => {
  res.json({ ok: true, got: req.body });
});

// ── in-memory notes store ──────────────────────────────────────────
let nextId = 1;
const notes = new Map();

app.get('/api/notes', (_req, res) => {
  res.json({ items: [...notes.values()].sort((a, b) => b.id - a.id) });
});

app.post('/api/notes', (req, res) => {
  const title = String(req.body?.title ?? '').trim();
  if (!title) {
    return res.status(400).json({ error: 'title is required' });
  }
  const note = { id: nextId++, title, body: String(req.body?.body ?? ''), at: Date.now() };
  notes.set(note.id, note);
  res.status(201).json(note);
});

app.delete('/api/notes/:id', (req, res) => {
  const id = Number(req.params.id);
  if (!notes.has(id)) {
    return res.status(404).json({ error: 'not found' });
  }
  notes.delete(id);
  res.status(204).end();
});

const server = app.listen(port, '127.0.0.1', () => {
  // alex.ready handshake — host reads this off stderr and uses
  // the port to wire the alex://app/api/* reverse proxy.
  process.stderr.write(JSON.stringify({ type: 'alex.ready', port }) + '\n');
});
