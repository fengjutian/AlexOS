// Service-mode backend. The host allocates a port from
// 28000-28999 (or honors `manifest.backend.port` if set) and
// injects it as `ALEX_SERVICE_PORT`. We bind to 127.0.0.1
// on that exact port and report it back in the `alex.ready`
// line on stderr so the host can wire the alex://app/api/*
// reverse proxy.
const express = require('express');

const port = Number(process.env.ALEX_SERVICE_PORT);
if (!Number.isInteger(port) || port <= 0) {
  process.stderr.write(`invalid ALEX_SERVICE_PORT: ${process.env.ALEX_SERVICE_PORT}\n`);
  process.exit(2);
}

const app = express();
app.use(express.json());

app.get('/health', (_req, res) => res.status(200).send('ok'));

app.get('/api/hello', (_req, res) => {
  res.json({ ok: true, msg: 'hello from express', at: Date.now() });
});

app.post('/api/echo', (req, res) => {
  res.json({ ok: true, got: req.body });
});

app.get('/api/items', (_req, res) => {
  res.json({
    items: [
      { id: 1, name: 'apple' },
      { id: 2, name: 'banana' },
      { id: 3, name: 'cherry' },
    ],
  });
});

const server = app.listen(port, '127.0.0.1', () => {
  // alex.ready handshake — host reads this JSON line off stderr
  // and validates `port` matches the value it allocated.
  process.stderr.write(JSON.stringify({ type: 'alex.ready', port }) + '\n');
});
