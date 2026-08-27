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
| `filesystem.readText` | `filesystem.readText` | `filesystem.read` | file | stable | blocking |
| `filesystem.readBinary` | `filesystem.readBinary` | `filesystem.read` | file | stable | blocking |
| `filesystem.writeText` | `filesystem.writeText` | `filesystem.write` | file | stable | blocking |
| `filesystem.writeBinary` | `filesystem.writeBinary` | `filesystem.write` | file | stable | blocking |
| `filesystem.exists` | `filesystem.exists` | `filesystem.read` | file | stable | blocking |
| `filesystem.stat` | `filesystem.stat` | `filesystem.read` | file | stable | blocking |
| `filesystem.readDir` | `filesystem.readDir` | `filesystem.read` | file | stable | blocking |
| `filesystem.createDir` | `filesystem.createDir` | `filesystem.write` | file | stable | blocking |
| `filesystem.remove` | `filesystem.remove` | `filesystem.delete` | file | stable | blocking |
| `filesystem.rename` | `filesystem.rename` | `filesystem.write` + `filesystem.delete` | file | stable | blocking |
| `filesystem.copy` | `filesystem.copy` | `filesystem.read` + `filesystem.write` | file | stable | blocking |
| `filesystem.watch` | `filesystem.watch` | `filesystem.watch` | file | stable | blocking |
| `filesystem.unwatch` | `filesystem.unwatch` | none | file | stable | blocking |
| `storage.get` | `storage.get` | `storage` | storage-entry | stable | blocking |
| `storage.set` | `storage.set` | `storage` | storage-entry | stable | blocking |
| `storage.delete` | `storage.delete` | `storage` | storage-entry | stable | blocking |
| `storage.clear` | `storage.clear` | `storage` | storage-entry | stable | blocking |
| `storage.keys` | `storage.keys` | `storage` | storage-entry | stable | blocking |
| `paths.dataDir` | `paths.dataDir` | `paths` | path | stable | inline |
| `paths.cacheDir` | `paths.cacheDir` | `paths` | path | stable | inline |
| `paths.tempDir` | `paths.tempDir` | `paths` | path | stable | inline |
| `dialog.openFile` | `dialog.openFile` | `dialog.open` | dialog-selection | stable | native |
| `dialog.openFiles` | `dialog.openFiles` | `dialog.open` | dialog-selection | stable | native |
| `dialog.openDirectory` | `dialog.openDirectory` | `dialog.open` | dialog-selection | stable | native |
| `dialog.saveFile` | `dialog.saveFile` | `dialog.save` | dialog-selection | stable | native |
| `clipboard.readText` | `clipboard.readText` | `clipboard.read` | clipboard | stable | native |
| `clipboard.writeText` | `clipboard.writeText` | `clipboard.write` | clipboard | stable | native |
| `system.info` | `system.info` | none | host | stable | blocking |
| `system.capabilities` | `system.capabilities` | none | host | stable | blocking |
| `system.requestPermission` | `system.requestPermission` | none | host | stable | blocking |
| `system.openExternal` | `system.openExternal` | `system.openExternal` | host | stable | blocking |
| `system.listApps` | `system.listApps` | `system.manageApps` | host | stable | blocking |
| `system.listExtensions` | `system.listExtensions` | `system.manageExtensions` | host | stable | blocking |
| `system.install` | `system.install` | `system.install` | host | stable | blocking |
| `system.uninstall` | `system.uninstall` | `system.uninstall` | host | stable | blocking |
| `system.updateStart` | `system.updateStart` | `system.manageApps` | host | stable | blocking |
| `system.updateTasks` | `system.updateTasks` | `system.manageApps` | host | stable | blocking |
| `system.updateCancel` | `system.updateCancel` | `system.manageApps` | host | stable | blocking |
| `system.updateRetry` | `system.updateRetry` | `system.manageApps` | host | stable | blocking |
| `system.listPermissions` | `system.listPermissions` | `system.managePermissions` | host | stable | blocking |
| `system.setPermission` | `system.setPermission` | `system.managePermissions` | host | stable | blocking |
| `system.listTrustedPublishers` | `system.listTrustedPublishers` | `system.manageApps` | host | stable | blocking |
| `system.readAuditLog` | `system.readAuditLog` | `system.managePermissions` | host | stable | blocking |
| `window.setTitle` | `window.setTitle` | `window.manage` | window | stable | native |
| `window.minimize` | `window.minimize` | `window.manage` | window | stable | native |
| `window.maximize` | `window.maximize` | `window.manage` | window | stable | native |
| `window.close` | `window.close` | `window.manage` | window | stable | native |
| `notification.show` | `notification.show` | `notification.show` | notification | stable | native |
| `runtime.invoke` | `runtime.invoke` | `runtime.invoke` | runtime | stable | blocking |
| `runtime.status` | `runtime.status` | `runtime.manage` | runtime | stable | blocking |
| `runtime.restart` | `runtime.restart` | `runtime.manage` | runtime | stable | blocking |
| `runtime.cancel` | `runtime.cancel` | `runtime.invoke` | runtime | stable | blocking |
| `stream.credit` | `stream.credit` | `runtime.invoke` | stream | stable | blocking |
| `stream.read` | `stream.read` | `runtime.invoke` | stream | stable | blocking |
| `stream.cancel` | `stream.cancel` | `runtime.invoke` | stream | stable | blocking |
| `mcp.connections` | `mcp.connections` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.health` | `mcp.health` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.discover` | `mcp.discover` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.listTools` | `mcp.listTools` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.callTool` | `mcp.callTool` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.callToolInteractive` | `mcp.callToolInteractive` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.respondInput` | `mcp.respondInput` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.presentInput` | `mcp.presentInput` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.oauthBegin` | `mcp.oauthBegin` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.oauthLoopback` | `mcp.oauthLoopback` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.oauthComplete` | `mcp.oauthComplete` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.audit` | `mcp.audit` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.listResources` | `mcp.listResources` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.readResource` | `mcp.readResource` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.listPrompts` | `mcp.listPrompts` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.getPrompt` | `mcp.getPrompt` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.complete` | `mcp.complete` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.ping` | `mcp.ping` | `mcp.use` | mcp-binding | stable | blocking |
| `mcp.listen` | `mcp.listen` | `mcp.use` | mcp-binding | stable | blocking |
| `model.list` | `model.list` | `model.use` | model | stable | blocking |
| `model.import` | `model.import` | `model.manage` | model | stable | blocking |
| `model.downloadStart` | `model.downloadStart` | `model.manage` | model | stable | blocking |
| `model.downloadList` | `model.downloadList` | `model.manage` | model | stable | blocking |
| `model.downloadStatus` | `model.downloadStatus` | `model.manage` | model | stable | blocking |
| `model.downloadPause` | `model.downloadPause` | `model.manage` | model | stable | blocking |
| `model.downloadResume` | `model.downloadResume` | `model.manage` | model | stable | blocking |
| `model.hardware` | `model.hardware` | `model.use` | model | stable | blocking |
| `model.runtimeStatus` | `model.runtimeStatus` | `model.use` | model | stable | blocking |
| `model.workerPackages` | `model.workerPackages` | `model.manage` | model | stable | blocking |
| `model.workerInstall` | `model.workerInstall` | `model.manage` | model | stable | blocking |
| `model.workerActivate` | `model.workerActivate` | `model.manage` | model | stable | blocking |
| `model.remove` | `model.remove` | `model.manage` | model | stable | blocking |
| `model.load` | `model.load` | `model.manage` | model | stable | blocking |
| `model.unload` | `model.unload` | `model.manage` | model | stable | blocking |
| `model.cancel` | `model.cancel` | `model.use` | model | stable | blocking |
| `model.generate` | `model.generate` | `model.use` | model | stable | blocking |
| `model.embed` | `model.embed` | `model.use` | model | stable | blocking |
| `model.providers` | `model.providers` | `model.manage` | model | stable | blocking |
| `model.providerUpsert` | `model.providerUpsert` | `model.manage` | model | stable | blocking |
| `model.providerRemove` | `model.providerRemove` | `model.manage` | model | stable | blocking |
| `model.providerHealth` | `model.providerHealth` | `model.manage` | model | stable | blocking |
| `model.secretSet` | `model.secretSet` | `model.manage` | model | stable | blocking |
| `model.secretDelete` | `model.secretDelete` | `model.manage` | model | stable | blocking |
| `model.secretExists` | `model.secretExists` | `model.manage` | model | stable | blocking |
| `agent.create` | `agent.create` | `agent.run` | agent-run | stable | blocking |
| `agent.spawnChild` | `agent.spawnChild` | `agent.run` | agent-run | stable | blocking |
| `agent.children` | `agent.children` | `agent.run` | agent-run | stable | blocking |
| `agent.waitChildren` | `agent.waitChildren` | `agent.run` | agent-run | stable | blocking |
| `agent.schedule` | `agent.schedule` | `agent.run` | agent-run | stable | blocking |
| `agent.scheduled` | `agent.scheduled` | `agent.run` | agent-run | stable | blocking |
| `agent.start` | `agent.start` | `agent.run` | agent-run | stable | blocking |
| `agent.pause` | `agent.pause` | `agent.run` | agent-run | stable | blocking |
| `agent.resume` | `agent.resume` | `agent.run` | agent-run | stable | blocking |
| `agent.cancel` | `agent.cancel` | `agent.run` | agent-run | stable | blocking |
| `agent.approve` | `agent.approve` | `agent.run` | agent-run | stable | blocking |
| `agent.deny` | `agent.deny` | `agent.run` | agent-run | stable | blocking |
| `agent.status` | `agent.status` | `agent.run` | agent-run | stable | blocking |
| `agent.list` | `agent.list` | `agent.run` | agent-run | stable | blocking |
| `agent.history` | `agent.history` | `agent.run` | agent-run | stable | blocking |
| `agent.timeline` | `agent.timeline` | `agent.run` | agent-run | stable | blocking |
| `events.subscribe` | `events.subscribe` | none | event-subscription | stable | inline |
| `events.unsubscribe` | `events.unsubscribe` | none | event-subscription | stable | inline |
| `system.instances.create` | `system.instances.create` | `system.manageApps` | host | stable | blocking |
| `system.instances.start` | `system.instances.start` | `system.manageApps` | host | stable | blocking |
| `system.instances.stop` | `system.instances.stop` | `system.manageApps` | host | stable | blocking |
| `system.instances.restart` | `system.instances.restart` | `system.manageApps` | host | stable | blocking |
| `system.instances.remove` | `system.instances.remove` | `system.manageApps` | host | stable | blocking |
| `system.instances.inspect` | `system.instances.inspect` | `system.manageApps` | host | stable | blocking |
| `system.instances.list` | `system.instances.list` | `system.manageApps` | host | stable | blocking |
| `system.instances.logs` | `system.instances.logs` | `system.manageApps` | host | stable | blocking |
| `process.spawn` | `process.spawn` | `process.spawn` | process | stable | blocking |
| `process.kill` | `process.kill` | `process.spawn` | process | stable | blocking |
| `net.fetch` | `net.fetch` | `network.fetch` | network-origin | stable | blocking |
| `window.create` | `window.create` | `window.open` | window | stable | native |
| `window.list` | `window.list` | `window.open` | window | stable | native |
| `window.getBounds` | `window.getBounds` | `window.manage` | window | stable | native |
| `window.setBounds` | `window.setBounds` | `window.manage` | window | stable | native |
| `window.setFullscreen` | `window.setFullscreen` | `window.manage` | window | stable | native |
| `window.isFullscreen` | `window.isFullscreen` | `window.manage` | window | stable | native |
| `window.destroy` | `window.destroy` | `window.manage` | window | stable | native |
| `menu.setApplicationMenu` | `menu.setApplicationMenu` | `menu.manage` | menu | stable | native |
| `menu.setContextMenu` | `menu.setContextMenu` | `menu.manage` | menu | stable | native |
| `tray.create` | `tray.create` | `tray.manage` | tray | stable | native |
| `tray.destroy` | `tray.destroy` | `tray.manage` | tray | stable | native |
| `shortcuts.register` | `shortcuts.register` | `shortcut.register` | shortcut | stable | native |
| `shortcuts.unregister` | `shortcuts.unregister` | `shortcut.register` | shortcut | stable | native |
| `shortcuts.list` | `shortcuts.list` | `shortcut.register` | shortcut | stable | native |

Typed JSON Schema coverage: 142/142 methods.

Common errors: `INVALID_PARAMS`, `PERMISSION_DENIED`, `OPERATION_FAILED`, `DEADLINE_EXCEEDED`, `HOST_BUSY`, `METHOD_NOT_FOUND`.
