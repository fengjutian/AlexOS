import path from "node:path";
import { McpServer } from "./protocol.js";
import { buildToolList } from "./tools/registry.js";

const VERSION = "0.1.0";
const root = path.resolve(process.env["ALEX_WORKSPACE"] ?? "workspace");

new McpServer({
  serverName: "alex-filesystem",
  serverVersion: VERSION,
  root,
  tools: buildToolList(root),
});
