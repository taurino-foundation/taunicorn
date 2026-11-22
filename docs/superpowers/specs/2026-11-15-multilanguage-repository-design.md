# Multi-language repository structure — design

## Intent

Make Taunicorn a professional, reproducible Rust/Python/Node.js monorepo with one Git root and explicit, tested conventions for GitHub Actions, Cargo, uv and npm-compatible package publishing. "Rules" here means toolchain and repository conventions, not a claim of blanket legal compliance.

## Current state

- Cargo already has a root workspace containing the Rust core and Python/JavaScript bindings.
- Python packaging uses Maturin/PyO3, `pyproject.toml`, `uv.lock` and `python/`.
- The Node package is rooted at `package.json`, declares Yarn 4.18.1 and uses Yarn commands in CI, but both `yarn.lock` (v1 header) and `package-lock.json` (v3) exist.
- `crates/taunicorn-javascript/` has a nested, uncommitted `.git` repository, not a root-tracked submodule. Generated local native binaries are present in the worktree.
- GitHub CI and publish workflows exist for Rust and Python; JavaScript workflows are new in the current worktree and need alignment with the same least-privilege and reproducibility expectations.
- The JavaScript distribution already targets the public npm registry and generates platform-specific native packages through napi-rs; preserve that prebuilt-binary model.
- The language packages have distinct versions and release workflows (crates.io manual dispatch, PyPI release/dispatch, npm from the main-branch workflow). Preserve independent release units rather than imposing one synchronized version/tag.
- The working tree contains extensive pre-existing modifications and untracked files. Preserve their contents; do not reset or overwrite them.

## Goals

- Establish the root repository as the only Git boundary while preserving all language source and public package paths.
- Keep Yarn 4 as the sole Node dependency manager, with a valid immutable lockfile; retain npm-compatible package metadata, `npm pack` checks and registry publishing.
- Make committed lockfiles and workspace/package metadata deliberate and reproducible for Cargo, uv and Yarn.
- Give each language one documented local validation path and ensure CI triggers cover its source, manifests, lockfiles and workflows.
- Apply GitHub Actions least-privilege permissions, safe checkout behavior, pinned action revisions where practical, and explicit release boundaries.
- Ignore generated local build artifacts without hiding source, lockfiles, or distributable package files.

## Non-goals

- No IPC transport/security implementation changes, public API changes, protocol changes, or package-version changes.
- No exhaustive legal opinion or certification; project configuration cannot establish compliance with every law or registry policy.
- No package-directory moves unless required to fix an actual broken package boundary.
- No changes to pre-existing user work unrelated to the agreed cleanup.

## Approaches considered

1. **Root monorepo + Yarn 4 (chosen):** preserve the existing Cargo/Python/Node layout; remove the accidental nested Git metadata, make Yarn's lockfile authoritative, and align packaging/docs/CI. This best matches the declared `packageManager`, `.yarnrc.yml`, vendored Yarn executable and current workflows with the least source churn.
2. **Root monorepo + npm:** remove Yarn configuration and use npm lock/install commands throughout. Valid, but contradicts the current declared manager and requires broader lock/CI churn.
3. **Separate JavaScript repository:** formalize a submodule or independent release repository. This adds a repository boundary and synchronization overhead; it conflicts with the stated monorepo goal.

## Proposed implementation

1. Remove only `crates/taunicorn-javascript/.git` metadata after confirming the contents remain intact; do not remove or rewrite that crate's source.
2. Make the Node package's declared Yarn 4 manager and ESM/CJS export metadata explicit; migrate Yarn's lock, then remove the redundant npm lock only after immutable resolution succeeds. Replace the copied generic NAPI `plus100` sample test with a real public-API local duplex test. Keep npm-compatible manifest fields and add a package-content dry run.
3. Correct repository ignore/attribute rules for generated Python/Rust/Node artifacts while ensuring the root `Cargo.lock`, `uv.lock`, and canonical `yarn.lock` remain trackable.
4. Keep source layout and public paths; add concise root contributor/development guidance with Cargo, uv, Yarn and package-validation commands.
5. Align existing CI/security/release workflows: complete path coverage, locked installs, least-privilege token permissions, safe checkout credentials, consistent action pinning/update policy, and separate build/test/publish trust boundaries. Do not fabricate action SHAs; use verified revisions.
6. Verify Cargo workspace checks/tests, Python lock/build/tests, Yarn immutable install/lint/test/build, npm package dry-run, and workflow syntax. Report platform or toolchain checks that cannot be run locally.

## Risks and constraints

- The worktree is already dirty; modifications must be surgical, and the existing work must remain untouched.
- The local `yarn` command is not on PATH, though a repository-pinned Yarn 4 executable exists. Use that exact executable for validation; do not silently install or upgrade tooling.
- Action SHA pinning and release configuration must be verified against current upstream action releases before editing.
- Deleting redundant lock metadata is conditional on successfully generating and validating the canonical lockfile.
