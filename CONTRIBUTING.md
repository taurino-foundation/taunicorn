# Contributing to Taunicorn

Taunicorn is a single Git repository with a Rust/Tokio transport, native Python and Node.js bindings, and language-specific public packages. Keep the raw transport contract and package APIs stable unless a change explicitly requires a versioned migration.

## Repository map

- `crates/taunicorn/`: shared Rust transport and secure-message implementation.
- `crates/taunicorn-python/`, `python/`: PyO3 binding and Python package/tests.
- `crates/taunicorn-javascript/`, `javascript/`: napi-rs binding and npm package files.
- `examples/`, `benchmark/`: language examples and Node benchmarks.
- `.github/workflows/`: CI, security and independent publishing workflows.

## Toolchains

- Rust stable (see CI); keep the root `Cargo.lock` committed.
- Python 3.9 or newer; use `uv` and the committed `uv.lock`.
- Node.js 20 or newer; use repository-pinned Yarn 4.18.1 via `.yarn/releases/yarn-4.18.1.cjs`. Do not generate or commit a second npm lockfile.

## Local checks

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

uv lock --check
uv run --locked --extra test python -m pytest -q
uv build

node .yarn/releases/yarn-4.18.1.cjs install --immutable
node .yarn/releases/yarn-4.18.1.cjs lint
node .yarn/releases/yarn-4.18.1.cjs test
node .yarn/releases/yarn-4.18.1.cjs build
node .yarn/releases/yarn-4.18.1.cjs npm audit --all --recursive
npm pack --dry-run
```

Run language-specific checks when changing only one package, and run the relevant full checks before submitting a cross-language or workspace change. CI is authoritative for platform-specific wheels and native addon artifacts; do not commit locally built `.pyd`, `.node`, or Rust `target/` output.

## Releases and dependency policy

See [the delivery runbook](docs/DELIVERY.md) for staging, the production approval checklist, and rollback.

The Rust crate, Python distribution and npm package have independent versions and release workflows. Use the matching workflow and protected registry environment; a GitHub release or version change does not imply that every ecosystem should publish. Keep dependency lockfiles in sync with their declared manager, and do not bypass dependency audits by suppressing advisories without review.
