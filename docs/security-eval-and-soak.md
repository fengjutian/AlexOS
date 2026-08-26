---
layout: default
title: 安全评估与稳定性
parent: 架构与设计
nav_order: 12
---

# Security eval, soak and fault-injection tests

The default security evaluation is deterministic and offline:

```powershell
cargo test --test security_eval
```

It covers obfuscated prompt injection, benign false-positive samples, sensitive
data classification, oversized/deep MCP output and repeated classifier calls.
The default soak loop runs 10,000 iterations. Longer CI or release-candidate
runs can raise the bounded iteration count without changing the test binary:

```powershell
$env:ALEX_SOAK_ITERATIONS = "5000000"
cargo test --release --test security_eval content_filters_remain_deterministic_under_soak
```

Worker package tests inject modified signatures and corrupted active-version
pointers. The pointer test ensures a damaged state file cannot escape the
signed worker's `versions` directory. Model download and Agent Runtime unit
tests separately cover interrupted download recovery, interrupted
non-idempotent tool approval, daemon reopen and parent/child cancellation.

Release automation should run the default suite on every change and the
release-mode extended soak before signing Worker catalogs.
