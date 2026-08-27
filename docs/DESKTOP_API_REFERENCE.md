---
layout: default
title: Desktop API Reference
parent: 参考手册
nav_order: 2
---

# Desktop API reference

Generated from `packages/sdk/desktop-api.schema.json`. Do not edit manually.

| Method | Action | Permission | Resource | Maturity | Execution |
|---|---|---|---|---|---|
| `filesystem.readText` | `filesystem.readText` | filesystem.* | file | stable | blocking |
| `filesystem.readBinary` | `filesystem.readBinary` | filesystem.* | file | stable | blocking |
| `filesystem.writeText` | `filesystem.writeText` | filesystem.* | file | stable | blocking |
| `filesystem.writeBinary` | `filesystem.writeBinary` | filesystem.* | file | stable | blocking |
| `filesystem.exists` | `filesystem.exists` | filesystem.* | file | stable | blocking |
| `filesystem.stat` | `filesystem.stat` | filesystem.* | file | stable | blocking |
| `filesystem.readDir` | `filesystem.readDir` | filesystem.* | file | stable | blocking |
| `filesystem.createDir` | `filesystem.createDir` | filesystem.* | file | stable | blocking |
| `filesystem.remove` | `filesystem.remove` | filesystem.* | file | stable | blocking |
| `filesystem.rename` | `filesystem.rename` | filesystem.* | file | stable | blocking |
| `filesystem.copy` | `filesystem.copy` | filesystem.* | file | stable | blocking |
| `filesystem.watch` | `filesystem.watch` | filesystem.* | file | stable | blocking |
| `filesystem.unwatch` | `filesystem.unwatch` | filesystem.* | file | stable | blocking |
| `storage.get` | `storage.get` | storage | storage-entry | stable | blocking |
| `storage.set` | `storage.set` | storage | storage-entry | stable | blocking |
| `storage.delete` | `storage.delete` | storage | storage-entry | stable | blocking |
| `storage.clear` | `storage.clear` | storage | storage-entry | stable | blocking |
| `storage.keys` | `storage.keys` | storage | storage-entry | stable | blocking |
| `paths.dataDir` | `paths.dataDir` | none | path | stable | inline |
| `paths.cacheDir` | `paths.cacheDir` | none | path | stable | inline |
| `paths.tempDir` | `paths.tempDir` | none | path | stable | inline |
| `dialog.openFile` | `dialog.openFile` | dialog.* | dialog-selection | stable | native |
| `dialog.openFiles` | `dialog.openFiles` | dialog.* | dialog-selection | stable | native |
| `dialog.openDirectory` | `dialog.openDirectory` | dialog.* | dialog-selection | stable | native |
| `dialog.saveFile` | `dialog.saveFile` | dialog.* | dialog-selection | stable | native |
| `clipboard.readText` | `clipboard.readText` | clipboard.* | clipboard | stable | native |
| `clipboard.writeText` | `clipboard.writeText` | clipboard.* | clipboard | stable | native |
| `system.info` | `system.info` | method-specific | host | stable | blocking |
| `system.capabilities` | `system.capabilities` | method-specific | host | stable | blocking |
| `system.requestPermission` | `system.requestPermission` | method-specific | host | stable | blocking |
| `system.openExternal` | `system.openExternal` | method-specific | host | stable | blocking |
| `system.listApps` | `system.listApps` | method-specific | host | stable | blocking |
| `system.listExtensions` | `system.listExtensions` | method-specific | host | stable | blocking |
| `system.install` | `system.install` | method-specific | host | stable | blocking |
| `system.uninstall` | `system.uninstall` | method-specific | host | stable | blocking |
| `system.updateStart` | `system.updateStart` | method-specific | host | stable | blocking |
| `system.updateTasks` | `system.updateTasks` | method-specific | host | stable | blocking |
| `system.updateCancel` | `system.updateCancel` | method-specific | host | stable | blocking |
| `system.updateRetry` | `system.updateRetry` | method-specific | host | stable | blocking |
| `system.listPermissions` | `system.listPermissions` | method-specific | host | stable | blocking |
| `system.setPermission` | `system.setPermission` | method-specific | host | stable | blocking |
| `system.listTrustedPublishers` | `system.listTrustedPublishers` | method-specific | host | stable | blocking |
| `system.readAuditLog` | `system.readAuditLog` | method-specific | host | stable | blocking |
| `window.setTitle` | `window.setTitle` | window.* | window | stable | native |
| `window.minimize` | `window.minimize` | window.* | window | stable | native |
| `window.maximize` | `window.maximize` | window.* | window | stable | native |
| `window.close` | `window.close` | window.* | window | stable | native |
| `notification.show` | `notification.show` | notification | notification | stable | native |
| `runtime.invoke` | `runtime.invoke` | runtime | runtime | stable | blocking |
| `runtime.status` | `runtime.status` | runtime | runtime | stable | blocking |
| `runtime.restart` | `runtime.restart` | runtime | runtime | stable | blocking |
| `runtime.cancel` | `runtime.cancel` | runtime | runtime | stable | blocking |
| `stream.credit` | `stream.credit` | runtime.invoke | stream | stable | blocking |
| `stream.read` | `stream.read` | runtime.invoke | stream | stable | blocking |
| `stream.cancel` | `stream.cancel` | runtime.invoke | stream | stable | blocking |
| `mcp.connections` | `mcp.connections` | mcp.use | mcp-binding | stable | blocking |
| `mcp.health` | `mcp.health` | mcp.use | mcp-binding | stable | blocking |
| `mcp.discover` | `mcp.discover` | mcp.use | mcp-binding | stable | blocking |
| `mcp.listTools` | `mcp.listTools` | mcp.use | mcp-binding | stable | blocking |
| `mcp.callTool` | `mcp.callTool` | mcp.use | mcp-binding | stable | blocking |
| `mcp.callToolInteractive` | `mcp.callToolInteractive` | mcp.use | mcp-binding | stable | blocking |
| `mcp.respondInput` | `mcp.respondInput` | mcp.use | mcp-binding | stable | blocking |
| `mcp.presentInput` | `mcp.presentInput` | mcp.use | mcp-binding | stable | blocking |
| `mcp.oauthBegin` | `mcp.oauthBegin` | mcp.use | mcp-binding | stable | blocking |
| `mcp.oauthLoopback` | `mcp.oauthLoopback` | mcp.use | mcp-binding | stable | blocking |
| `mcp.oauthComplete` | `mcp.oauthComplete` | mcp.use | mcp-binding | stable | blocking |
| `mcp.audit` | `mcp.audit` | mcp.use | mcp-binding | stable | blocking |
| `mcp.listResources` | `mcp.listResources` | mcp.use | mcp-binding | stable | blocking |
| `mcp.readResource` | `mcp.readResource` | mcp.use | mcp-binding | stable | blocking |
| `mcp.listPrompts` | `mcp.listPrompts` | mcp.use | mcp-binding | stable | blocking |
| `mcp.getPrompt` | `mcp.getPrompt` | mcp.use | mcp-binding | stable | blocking |
| `mcp.complete` | `mcp.complete` | mcp.use | mcp-binding | stable | blocking |
| `mcp.ping` | `mcp.ping` | mcp.use | mcp-binding | stable | blocking |
| `mcp.listen` | `mcp.listen` | mcp.use | mcp-binding | stable | blocking |
| `model.list` | `model.list` | method-specific | model | stable | blocking |
| `model.import` | `model.import` | method-specific | model | stable | blocking |
| `model.downloadStart` | `model.downloadStart` | method-specific | model | stable | blocking |
| `model.downloadList` | `model.downloadList` | method-specific | model | stable | blocking |
| `model.downloadStatus` | `model.downloadStatus` | method-specific | model | stable | blocking |
| `model.downloadPause` | `model.downloadPause` | method-specific | model | stable | blocking |
| `model.downloadResume` | `model.downloadResume` | method-specific | model | stable | blocking |
| `model.hardware` | `model.hardware` | method-specific | model | stable | blocking |
| `model.runtimeStatus` | `model.runtimeStatus` | method-specific | model | stable | blocking |
| `model.workerPackages` | `model.workerPackages` | method-specific | model | stable | blocking |
| `model.workerInstall` | `model.workerInstall` | method-specific | model | stable | blocking |
| `model.workerActivate` | `model.workerActivate` | method-specific | model | stable | blocking |
| `model.remove` | `model.remove` | method-specific | model | stable | blocking |
| `model.load` | `model.load` | method-specific | model | stable | blocking |
| `model.unload` | `model.unload` | method-specific | model | stable | blocking |
| `model.cancel` | `model.cancel` | method-specific | model | stable | blocking |
| `model.generate` | `model.generate` | method-specific | model | stable | blocking |
| `model.embed` | `model.embed` | method-specific | model | stable | blocking |
| `model.providers` | `model.providers` | method-specific | model | stable | blocking |
| `model.providerUpsert` | `model.providerUpsert` | method-specific | model | stable | blocking |
| `model.providerRemove` | `model.providerRemove` | method-specific | model | stable | blocking |
| `model.providerHealth` | `model.providerHealth` | method-specific | model | stable | blocking |
| `model.secretSet` | `model.secretSet` | method-specific | model | stable | blocking |
| `model.secretDelete` | `model.secretDelete` | method-specific | model | stable | blocking |
| `model.secretExists` | `model.secretExists` | method-specific | model | stable | blocking |
| `agent.create` | `agent.create` | agent.run | agent-run | stable | blocking |
| `agent.spawnChild` | `agent.spawnChild` | agent.run | agent-run | stable | blocking |
| `agent.children` | `agent.children` | agent.run | agent-run | stable | blocking |
| `agent.waitChildren` | `agent.waitChildren` | agent.run | agent-run | stable | blocking |
| `agent.schedule` | `agent.schedule` | agent.run | agent-run | stable | blocking |
| `agent.scheduled` | `agent.scheduled` | agent.run | agent-run | stable | blocking |
| `agent.start` | `agent.start` | agent.run | agent-run | stable | blocking |
| `agent.pause` | `agent.pause` | agent.run | agent-run | stable | blocking |
| `agent.resume` | `agent.resume` | agent.run | agent-run | stable | blocking |
| `agent.cancel` | `agent.cancel` | agent.run | agent-run | stable | blocking |
| `agent.approve` | `agent.approve` | agent.run | agent-run | stable | blocking |
| `agent.deny` | `agent.deny` | agent.run | agent-run | stable | blocking |
| `agent.status` | `agent.status` | agent.run | agent-run | stable | blocking |
| `agent.list` | `agent.list` | agent.run | agent-run | stable | blocking |
| `agent.history` | `agent.history` | agent.run | agent-run | stable | blocking |
| `agent.timeline` | `agent.timeline` | agent.run | agent-run | stable | blocking |
| `events.subscribe` | `events.subscribe` | none | event-subscription | stable | inline |
| `events.unsubscribe` | `events.unsubscribe` | none | event-subscription | stable | inline |
| `system.instances.create` | `system.instances.create` | method-specific | host | stable | blocking |
| `system.instances.start` | `system.instances.start` | method-specific | host | stable | blocking |
| `system.instances.stop` | `system.instances.stop` | method-specific | host | stable | blocking |
| `system.instances.restart` | `system.instances.restart` | method-specific | host | stable | blocking |
| `system.instances.remove` | `system.instances.remove` | method-specific | host | stable | blocking |
| `system.instances.inspect` | `system.instances.inspect` | method-specific | host | stable | blocking |
| `system.instances.list` | `system.instances.list` | method-specific | host | stable | blocking |
| `system.instances.logs` | `system.instances.logs` | method-specific | host | stable | blocking |
| `process.spawn` | `process.spawn` | process.* | process | stable | blocking |
| `process.kill` | `process.kill` | process.* | process | stable | blocking |
| `net.fetch` | `net.fetch` | network.fetch | network-origin | stable | blocking |
| `window.create` | `window.create` | window.* | window | stable | native |
| `window.list` | `window.list` | window.* | window | stable | native |
| `window.getBounds` | `window.getBounds` | window.* | window | stable | native |
| `window.setBounds` | `window.setBounds` | window.* | window | stable | native |
| `window.setFullscreen` | `window.setFullscreen` | window.* | window | stable | native |
| `window.isFullscreen` | `window.isFullscreen` | window.* | window | stable | native |
| `window.destroy` | `window.destroy` | window.* | window | stable | native |
| `menu.setApplicationMenu` | `menu.setApplicationMenu` | menu | menu | stable | native |
| `menu.setContextMenu` | `menu.setContextMenu` | menu | menu | stable | native |
| `tray.create` | `tray.create` | tray | tray | stable | native |
| `tray.destroy` | `tray.destroy` | tray | tray | stable | native |
| `shortcuts.register` | `shortcuts.register` | shortcuts | shortcut | stable | native |
| `shortcuts.unregister` | `shortcuts.unregister` | shortcuts | shortcut | stable | native |
| `shortcuts.list` | `shortcuts.list` | shortcuts | shortcut | stable | native |

Typed JSON Schema coverage: 142/142 methods.

Common errors: `INVALID_PARAMS`, `PERMISSION_DENIED`, `OPERATION_FAILED`, `DEADLINE_EXCEEDED`, `HOST_BUSY`, `METHOD_NOT_FOUND`.
