# Sui Indexer Agent Instructions

## Non-negotiables

- Surgical edits only: every changed line must trace to the request. No drive-by refactors, no reformatting untouched code, no deleting pre-existing dead code (mention it instead).
- Fix root causes. Never silence a warning, weaken a check, or patch a symptom to make a gate pass.
- Never report completion while any `just` gate below fails. Report the failure verbatim and stop.
- Stop and ask when a request is ambiguous, conflicts with existing conventions, or requires credentials, publishing, or destructive operations.
- When a correction reveals a missing rule, add one specific rule here ("always X for Y", never "be careful with Y"); delete rules that never catch real errors.

## Scope

- Rust workspace for a modular blockchain indexer. `bin/sui-indexer-cli` is the production CLI (published binary name: `sui-indexer`).
- Crates, bottom-up: `sui-indexer-config` (TOML + env config), `chain-core` (chain-neutral height cursor, sync, reorg, decoded-row contract), `adapter-move` (Sui checkpoints), `adapter-svm` (Solana slots), `sui-indexer-events` (batch/filter/transformer pipeline), `sui-indexer-storage` (PostgreSQL via `sqlx` plus `crates/sui-indexer-storage/migrations/`), `job-engine` (N user-defined indexing jobs), `rule-host` (hot-loadable WASM rules), `archive-store` (ClickHouse archive of record), `sui-indexer-core` (orchestration: `IndexerCore`, sync engine, sinks, repair, jobs).
- `config.example.toml` documents the config surface; `docs/multichain-job-engine.md` is the design proposal behind `chain-core`/`job-engine`/`rule-host`; `examples/` shows the library API.
- English only in code, comments, docs, and commit messages.

## Execution

- Parallelize aggressively: dispatch subagents for independent research, exploration, and implementation.
- NEVER run two `cargo` commands concurrently, and never overlap file edits with a running `cargo` command (`target/` file lock): finish edits, wait, then run.
- NEVER use `git worktree`. Edit the current branch in place.
- TDD for behavior changes: failing test first, smallest change to pass, then run `just mutation` and fix surviving mutants.
- The `sui-*` git dependencies pull and compile a large slice of the Sui monorepo, so a full `cargo build`/`cargo check`/`cargo test` is slow. Use `just check-fast` (`cargo metadata --no-deps`) for fast structural verification while iterating.

## Verification loop

After each feature or bug fix, in order; any failure means the task is not done:

```bash
just format
just lint
just test
just mutation
```

This repo has no CI workflow, so `just lint` is the gate: typos, rumdl, `taplo fmt --check`, `cargo sort -c`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo shear`, workspace-inheritance check. `just test` needs no database — the `*_live.rs` tests skip themselves unless `DATABASE_URL` points at a scratch PostgreSQL. Run `just setup` once to install the tools `just lint` needs.

## Rust rules

- Toolchain is pinned to stable `1.96.1` in `rust-toolchain.toml`; the Justfile uses plain `cargo fmt` / `cargo clippy`. Do not introduce `+nightly` or `+1.96.1` variants.
- All `sui-*` dependencies are git deps from `MystenLabs/sui` pinned to ONE `rev`, repeated on every `sui-*` entry in `[workspace.dependencies]`. Two different revs compile two copies of `sui-types` and produce "expected `sui_types::…`, found `sui_types::…`" mismatches. Find a new rev with `git ls-remote https://github.com/MystenLabs/sui.git HEAD`.
- Never hand-type dependency versions. Shared deps live in root `[workspace.dependencies]` (numeric versions, no features) and are referenced with `dep = { workspace = true }`; add them with `cargo add <crate> --workspace` and enable features in the member crate.
- Every library crate sets `[lints] workspace = true`. The shared set denies `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr` and bare `#[allow]`. `unwrap_used`/`expect_used` are `allow` here — do not tighten them without a migration plan. The CLI crate uses its own `[lints.clippy]` because it prints user-facing tables.
- `cargo shear` cannot see the Sui git deps that are only reached through re-exports; they are listed in `[workspace.metadata.cargo-shear] ignored`. If it flags a `sui-*` crate, add that exact name to the list — never delete the dependency.

## Dependency choices

- Errors: `thiserror` for concrete library error types, `eyre` for application glue, `anyhow` only in `rule-host` (the wasmtime host API). Never invent string errors.
- HTTP: `reqwest` (rustls) is already in the tree because the Sui SDK requires it. Do not add a second HTTP client.
- Database: `sqlx` + PostgreSQL. Runtime queries only (`sqlx::query_as` with `#[derive(sqlx::FromRow)]`); never the compile-time `sqlx::query!` macros — they need a live database at build time. Schema changes go into `crates/sui-indexer-storage/migrations/` as a new timestamped `.sql` file, never as an edit to an applied one.
- Logging: `tracing` only. `print_stdout`/`print_stderr` are denied in library code.
- Property tests: `proptest` lives in `adapter-move`, `adapter-svm`, `chain-core`, `job-engine` and `rule-host` and runs inside the normal `cargo test` loop. Do not add a separate property-test command.

## Code conventions

- Unit tests live in `#[cfg(test)]` modules next to the code; integration tests in crate-level `tests/`. Add a test for every behavior or public-API change.
- A job is DATA, not a process: job definitions live in the control-plane tables and `job-engine` reconciles desired state. A logic change means a new versioned output table with an atomic alias swap (`job-engine/src/outputs.rs`) — never an in-place `ALTER` of a live output.
- `chain-core` owns the chain-neutral contract (`ChainKind`, `DecodedBlock`, `ReorgMode`, height cursor). Chain-specific fetch and decode live in `adapter-*`; the sync engine must never learn chain types.
- `rule-host` speaks ONE encoding: JSON over a single wasmtime linear-memory call against a versioned ABI (`ABI_VERSION`). Rules never see Rust types; changing the ABI means bumping `ABI_VERSION`.
- Release lock guards before `.await`; use `tokio::sync::Mutex` only where the guard must span an `.await` (e.g. the shared Sui gRPC client in `adapter-move`).
- `cargo mutants` writes `mutants.out/`, which this repo does NOT git-ignore — do not commit it. Fix surviving mutants instead of widening the ignored set.
