# Rust Native Worker Protocol v1

Rust Native Worker 是 Alex Runtime 的通用进程外原生扩展边界。第三方原生代码不得加载到
Shell 或 Daemon 主进程；Host 从受信任的包目录启动 Worker，并通过 stdin/stdout 交换 UTF-8
JSON Lines。stderr 仅用于日志。

## 描述符

包内 `native-worker.json` 的最小结构：

```json
{
  "schemaVersion": 1,
  "id": "com.example.image-worker",
  "command": "bin/image-worker.exe",
  "args": [],
  "capabilities": ["image.resize"]
}
```

应用通过 Manifest v2 绑定 Worker：

```yaml
nativeWorkers:
  image:
    descriptor: native/native-worker.json
    resources:
      memoryMb: 256
      cpuPercent: 50
      processes: 1
```

`command` 必须是相对路径；规范化后的可执行文件必须仍位于包目录内。Host 不执行 PATH
查找，也不允许绝对路径。标识符只允许 ASCII 字母、数字、点、连字符和下划线。

## 帧与调用

请求：

```json
{"protocol":1,"requestId":"native-1","method":"image.resize","params":{"width":80}}
```

成功响应：

```json
{"protocol":1,"requestId":"native-1","result":{"path":"output.png"}}
```

失败响应：

```json
{"protocol":1,"requestId":"native-1","error":{"code":"INVALID_IMAGE","message":"unsupported format"}}
```

- 每帧以换行结尾，最大 1 MiB；
- 当前连接串行处理请求，响应必须匹配 `protocol` 和 `requestId`；
- 响应必须只包含 `result` 或 `error` 之一；
- 默认调用超时为 120 秒，也可以由 Host 为单次调用指定更短超时；
- 超时、EOF、非法 JSON 或协议身份不匹配会使 Worker 失效并被终止；
- Host 释放 Worker handle 时必须终止并回收子进程。

主动取消使用独立控制帧：

```json
{"protocol":1,"type":"cancel","requestId":"native-1"}
```

Named Pipe 的 `nativeWorkerCancel` 设置线程安全取消信号，不需要等待正在执行的 Worker 锁。
Host 将 cancel 帧写入同一 stdin，并允许 Worker 在 5 秒内返回当前请求的终止响应；逾期会关闭
Job 并强制回收 Worker。取消只作用于该 `(application, binding)` 当前正在执行的串行请求。

## 尚未接线

Daemon 已持有按 `(application, binding)` 隔离的 Worker Manager，支持启动、调用、状态、
停止、按应用清理和 shutdown 全量清理；调用方法必须精确匹配描述符声明的 capability。
Named Pipe v1 已开放 `nativeWorkerStart`、`nativeWorkerInvoke`、`nativeWorkerCancel`、
`nativeWorkerStatus` 和 `nativeWorkerStop`；调用超时限制为 1–120000 ms，默认 30000 ms。Daemon 从已安装应用目录
重新加载并验证 Manifest，不接受客户端提供的可执行路径。

签名安装和流式事件仍属于后续切片。Worker 启动后已通过统一隔离层绑定 Windows
Job Object；`memoryMb`、`processes` 和 `cpuPercent` 分别成为进程内存、进程树数量和 CPU
硬限制，Job handle 关闭会终止完整进程树。CPU rate 使用 Windows 的 1/100 百分比单位并启用
`HARD_CAP`；Manifest 只接受 1–100。`dataQuotaMb` 目前会进入隔离配置与状态，但尚未形成硬限制。
非 Windows 平台当前只提供进程生命周期管理，
并在状态中的 `isolated` 返回 `false`。模型推理 Worker 继续使用其专用协议，不由本通用协议替换。

Windows Native Worker Manager 已切换到 Restricted Token + stdio + Job Object 启动原语：
进程在削减可移除权限的主令牌下创建，同时保留 JSONL 管道，并在执行后立即加入资源受限、
关闭即回收的 Job。受限令牌或 Job 创建失败时启动会 fail-closed，不会回退到普通令牌。

Worker 环境不再继承 Daemon 的完整环境，只保留 `SystemRoot`、`WINDIR`、`TEMP`、`TMP`，并由
Host 注入 `ALEX_PACKAGE_ROOT`、`ALEX_APP_ID` 和 `ALEX_WORKER_BINDING`。非 Windows 路径同样
清空继承环境并注入对应的 `ALEX_*` 身份字段。

流式协议核心允许 Worker 在终止响应前发送任意数量的事件帧：

```json
{"protocol":1,"requestId":"native-1","event":{"type":"delta","text":"hello"}}
```

每个事件仍受 1 MiB 帧上限、协议版本和 `requestId` 校验。流式调用逐帧执行 Host 回调，回调
拒绝或协议损坏会终止 Worker；普通非流式调用收到事件会报协议错误。接入 Daemon 的信用流
`StreamManager` 与 Named Pipe 流式启动命令仍是下一切片。
