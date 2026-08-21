// Manually mimic what proxy_to_service would send, to isolate
// whether the 404 is in the backend, the proxy, or somewhere else.
const { spawn } = require("node:child_process");
const net = require("node:net");

const port = 28889;
const proc = spawn(
  "node",
  ["D:/github/AlexOS/examples/service-hello/backend/index.js"],
  {
    env: {
      ...process.env,
      ALEX_SERVICE_PORT: String(port),
      ALEX_APP_ID: "com.alex.service-hello",
      ALEX_RUNTIME_TOKEN: "deadbeefdeadbeefdeadbeefdeadbeef",
    },
    stdio: ["ignore", "pipe", "pipe"],
  }
);

proc.stderr.on("data", (b) => {
  process.stderr.write(`[backend] ${b}`);
  if (b.toString().includes("alex.ready")) {
    setTimeout(() => sendProxyRequest(port), 50);
  }
});

function sendProxyRequest(port) {
  const client = net.createConnection({ host: "127.0.0.1", port }, () => {
    // Mimic what proxy_to_service sends after the rebuild.
    const req =
      "GET /api/info HTTP/1.0\r\n" +
      "Host: 127.0.0.1\r\n" +
      "X-Alx-App-Id: com.alex.service-hello\r\n" +
      "Connection: close\r\n" +
      "Content-Length: 0\r\n" +
      "Accept: application/json\r\n" +
      "X-Alx-Token: deadbeefdeadbeefdeadbeefdeadbeef\r\n" +
      "\r\n";
    client.write(req);
    client.end();
  });
  let buf = "";
  client.on("data", (c) => (buf += c.toString()));
  client.on("end", () => {
    console.log("=== RAW RESPONSE ===");
    console.log(buf);
    proc.kill();
    process.exit(0);
  });
  client.on("error", (e) => {
    console.error("client error:", e.message);
    proc.kill();
    process.exit(1);
  });
}

setTimeout(() => {
  console.error("timeout");
  proc.kill();
  process.exit(2);
}, 5000);
