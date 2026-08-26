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

## 尚未接线

Daemon 生命周期控制、签名安装、能力授权、流式事件、主动取消，以及 Windows Job Object
的 CPU/内存/进程树强制仍属于后续切片。模型推理 Worker 继续使用其专用协议，不由本通用
协议替换。
