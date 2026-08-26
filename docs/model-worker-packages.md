# Model Worker packages

Alex distributes llama.cpp and ONNX Runtime GenAI adapters as signed, versioned
ZIP packages rather than linking inference libraries into the daemon.

Each archive contains a `worker.json` descriptor and its package-relative
executable. The signed catalog manifest binds the engine (`llama-cpp` or
`onnx-runtime-genai`), worker kind, semantic version, host target triple, HTTPS
URL, byte length, SHA-256 digest and publisher key.

Packages are installed below:

```text
runtimes/model-workers/<kind>/versions/<version>/<triple>/
```

`active.json` is the atomic version pointer. Installing or activating a version
starts a candidate process and reloads the models assigned to that worker before
the live registry is replaced. A failed download, signature check, extraction,
startup or model restore therefore leaves the previous worker online. Selecting
an older installed version through `model.workerActivate` is the rollback path.

Production downloads require the Ed25519 publisher key to be present in the
Alex publisher Trust Store. Archives reject traversal paths, excessive file
counts and more than 8 GiB of extracted data.
