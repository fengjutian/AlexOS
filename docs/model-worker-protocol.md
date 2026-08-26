---
layout: default
title: Model Worker Protocol
parent: 参考手册
nav_order: 5
---

# Local Model Worker Protocol v1

> 2026-08-25 修订：本文是 worker ↔ alexd 协议规范 v1。Runtime Provider 阶段（阶段九，参见
> [`ai-runtime-implementation.md`](./ai-runtime-implementation.md)）尚未实现；`src/model/`
> 当前以 `remote.rs` 远程 provider 为主，本地 worker 适配器未起。规范本身保持稳定。

Alex Runtime does not load inference libraries into the Shell or Daemon. A model
engine is hosted by a dedicated worker process and communicates with `alexd`
using newline-delimited JSON on stdin/stdout. Worker logs must use stderr.

## Installation and discovery

```text
runtimes/model-workers/<kind>/
  worker.json
  <executable and private dependencies>
```

Example `worker.json`:

```json
{
  "schemaVersion": 1,
  "kind": "llama-cpp",
  "command": "alex-llama-worker.exe",
  "args": ["--threads", "8"],
  "providers": ["cuda", "directMl", "cpu"],
  "maxConcurrency": 1,
  "memoryOverheadMb": 256,
  "memoryLimitMb": 8192
}
```

`kind` must match the directory name. The canonical executable path must stay
inside that worker directory. Desktop applications cannot register arbitrary
commands. The Daemon discovers and starts workers with the model subsystem.

## Framing and limits

- UTF-8 JSON Lines: one JSON object followed by `\n` per frame.
- Every frame contains `"protocol": 1`.
- stdin is requests, stdout is protocol responses/events, stderr is logs.
- Frames larger than 1 MiB are rejected.
- `load`, `generate`, and `unload` are serialized per worker process.
- `cancel` can arrive while `generate` is running.

## Operations

Load:

```json
{"protocol":1,"type":"load","model":{"id":"local/tiny@1"},"path":".../blob"}
{"protocol":1,"type":"loaded"}
```

The request contains the complete `ModelManifest`, not only the abbreviated
object shown above. Process workers also receive a daemon-selected placement:

```json
{"placement":{"deviceId":"gpu:GPU-123:cuda","provider":"cuda","reservedMemoryMb":6144}}
```

Alex prefers the least-utilized compatible device with enough current free
memory and retains a GPU safety margin. NVIDIA free memory and utilization are
sampled from `nvidia-smi`; CPU scheduling uses currently available physical
memory. The signed worker descriptor contributes its fixed runtime overhead.

Generate and events:

```json
{"protocol":1,"type":"generate","request":{"requestId":"req-1","model":"local/tiny@1","messages":[],"options":{}}}
{"protocol":1,"requestId":"req-1","event":{"type":"delta","text":"Hello"}}
{"protocol":1,"requestId":"req-1","event":{"type":"usage","input_tokens":3,"output_tokens":1}}
{"protocol":1,"requestId":"req-1","event":{"type":"finish","reason":"stop"}}
```

A tool-call event is also supported:

```json
{"protocol":1,"requestId":"req-1","event":{"type":"toolCall","name":"search","arguments":{"q":"Alex"}}}
```

Cancel is fire-and-forget. The worker must end the request with a `finish`
event, normally using reason `cancelled`:

```json
{"protocol":1,"type":"cancel","requestId":"req-1"}
```

Unload:

```json
{"protocol":1,"type":"unload","modelId":"local/tiny@1"}
{"protocol":1,"type":"unloaded"}
```

## Errors and lifecycle

```json
{"protocol":1,"error":{"code":"MODEL_LOAD_FAILED","message":"not enough memory"}}
```

Programmatic clients should use `error.code`; `message` is diagnostic text.
EOF, invalid JSON, protocol mismatch, an oversized frame, or a mismatched
`requestId` fails the active operation. Dropping the adapter terminates and
reaps the child process.

The v1 adapter provides process separation, streaming, cancellation and bounded
request/response frames. Alex discovers CPU/GPU/NPU capabilities, validates the
providers declared by each worker, accounts loaded model bytes against a daemon
memory budget, limits concurrent requests and evicts only idle least-recently-used
models. Every blocking response and every generation-event gap has a 120-second
deadline; expiry terminates the process. EOF, protocol corruption and timeout
failures trigger a clean respawn from the daemon-owned descriptor, and alexd
restores the models previously loaded by that worker before publishing the
replacement. Runtime status exposes worker PID and health. OS-level worker
memory enforcement is mandatory on Windows: `memoryLimitMb` becomes a hard
Job Object process-tree ceiling. The handle survives for the worker lifetime
and is recreated after a crash; dropping it terminates the complete tree.
