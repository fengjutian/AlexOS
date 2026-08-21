// Quick smoke test: start service-hello backend, curl /api/info.
const { spawn } = require("node:child_process");
const http = require("node:http");
const net = require("node:net");

const port = 28888;
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

let ready = false;
proc.stderr.on("data", (b) => {
  process.stderr.write(`[backend] ${b}`);
  if (b.toString().includes("alex.ready")) ready = true;
});
proc.on("exit", (code) => process.stderr.write(`[backend exit ${code}]\n`));

function tryFetch(retries = 20) {
  const req = http.request(
    { host: "127.0.0.1", port, path: "/api/info", method: "GET" },
    (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("end", () => {
        console.log(`STATUS: ${res.statusCode}`);
        console.log(`BODY: ${body}`);
        proc.kill();
        process.exit(0);
      });
    }
  );
  req.on("error", (err) => {
    if (retries > 0) setTimeout(() => tryFetch(retries - 1), 100);
    else {
      console.error(`fetch failed: ${err.message}`);
      proc.kill();
      process.exit(1);
    }
  });
  req.end();
}

setTimeout(() => tryFetch(), 200);
setTimeout(() => {
  console.error("timeout");
  proc.kill();
  process.exit(2);
}, 5000);
