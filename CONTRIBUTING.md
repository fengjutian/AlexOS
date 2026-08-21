# Contributing

## Local checks

Before pushing, run the same checks CI runs:

```powershell
cargo fmt --all -- --check
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
node --test packages/sdk/test/sdk.test.mjs
```

## CI

Two parallel jobs run on every push and PR:

- **Linux · fmt + clippy + test** — `ubuntu-latest`, runs every check above
- **Windows · build + test** — `windows-latest`, exercises WebView2-bound code paths

Both jobs cache `~/.cargo/registry`, `~/.cargo/git`, and `target` keyed on
`Cargo.lock` so cache invalidation tracks dependency changes, not source
changes.

## Layout

- `src/` — Rust core, organised by subsystem (`api/`, `manager/`, `runtime/`, etc.)
- `packages/sdk/` — `@alex/sdk` JavaScript + TypeScript package
- `tests/core.rs` — Integration tests (one binary, many `#[test]` functions)
- `.github/workflows/ci.yml` — CI definition

## Adding a new subsystem

1. Create `src/<name>.rs` (or `src/<name>/mod.rs` if it grows large)
2. Add `pub mod <name>;` to `src/lib.rs`
3. Add a dedicated error type if the subsystem does I/O
4. Cover the public surface with integration tests in `tests/core.rs`
5. Keep `clippy --all-targets -- -D warnings` clean
