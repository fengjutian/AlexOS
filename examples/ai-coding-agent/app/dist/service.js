import path from "node:path";
import { AppController } from "./controllers/app.js";
import { StdioRpcServer } from "./protocol/io.js";
import { WorkspaceService } from "./services/workspace.js";
import { logger } from "./util/logger.js";
const VERSION = "0.1.0";
const workspaceRoot = path.resolve(process.env["ALEX_WORKSPACE"] ?? "workspace");
const workspace = new WorkspaceService({
    root: workspaceRoot,
    startedAt: new Date(),
    version: VERSION,
    capabilities: ["app.info", "app.echo", "app.ping", "app.workspace.list", "app.workspace.read", "app.config.get"],
});
const controller = new AppController(workspace);
new StdioRpcServer((request) => controller.handle(request));
logger.info("service ready", { version: VERSION, workspace: workspaceRoot, pid: process.pid });
//# sourceMappingURL=service.js.map