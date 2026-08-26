---
layout: default
title: Native Worker 指南
nav_order: 10
---

# Rust Native Worker 指南

Native Worker 是独立进程扩展边界，不是加载到 Shell/Daemon 的动态库。协议细节见
[`native-worker-protocol.md`](./native-worker-protocol.md)。

## Descriptor

```json
{
  "schemaVersion": 1,
  "id": "com.example.image-worker",
  "command": "bin/image-worker.exe",
  "args": [],
  "capabilities": ["image.resize"]
}
```

`command` 必须是包内相对路径。Worker 从 stdin 读取 UTF-8 JSON Lines，将协议响应写到 stdout，
日志只能写 stderr。

## Manifest v2

```yaml
nativeWorkers:
  image:
    descriptor: native/native-worker.json
    resources:
      memoryMb: 256
      cpuPercent: 50
      processes: 1
```

## 生命周期

Daemon 按 `(application, binding)` 隔离实例，支持 start、invoke、stream、cancel、status、stop 和
restart。崩溃实例在再次 start 时会清除陈旧槽位；显式 restart 会先取消和回收旧进程，再重新加载
已安装 Manifest。当前没有自动重启退避与崩溃熔断。

## 安全边界

- Windows 使用 Restricted Token、stdio 和 Job Object，隔离创建失败时 fail closed；
- memory/processes/cpuPercent 映射为 Job 硬限制；
- Worker 只继承必要系统临时目录和 `ALEX_PACKAGE_ROOT`、`ALEX_APP_ID`、
  `ALEX_WORKER_BINDING`；
- 单帧最大 1 MiB；方法必须精确匹配 descriptor capability；
- cancel 给 Worker 5 秒收尾，逾期回收整个 Job。

## 调试

协议错误、EOF、requestId 不匹配和超时都会使 Worker 失效。确保 stdout 没有日志、banner 或调试
文本；使用 stderr 输出诊断。先运行 Native Worker 专项测试：

```powershell
cargo test --offline native_worker --lib
```
