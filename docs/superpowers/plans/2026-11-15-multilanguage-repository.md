# Multi-Language Monorepo Standardization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing Rust/Python/Node.js repository a reproducible monorepo with one Git boundary, one Node package manager, and tested Cargo, uv, npm-registry and GitHub Actions conventions.

**Architecture:** Keep the existing Cargo workspace and public Python/JavaScript package paths. Use the repository-pinned Yarn 4 for Node development and lock resolution, uv for Python project/lock operations, Cargo for Rust, and maintain independent package versions and release workflows.

**Tech Stack:** Rust/Cargo, Python/PyO3/Maturin/uv, TypeScript/JavaScript/napi-rs/Yarn 4/npm registry, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-15-multilanguage-repository-design.md`

## Global Constraints

- Preserve existing source files, public APIs, protocol behavior, package versions and independent release units.
- Preserve existing uncommitted user changes; do not reset, stage or commit user files.
- Keep `Cargo.lock`, `uv.lock` and the canonical Yarn lockfile trackable and reproducible.
- Yarn 4.18.1 is the only Node dependency manager; npm remains the publishing/package-inspection client for npmjs.org.
- Do not claim blanket legal compliance; enforce verifiable tool and repository policies only.
- Do not remove generated package artifacts required for npm distribution; ignore local build outputs only where safe.

## Review Focus

- Root `Cargo.lock` must remain tracked even after ignore-rule cleanup — verify with `git ls-files` and `git check-ignore`.
- Yarn 4 lock resolution must succeed immutably with no second npm lockfile — verify lockfile format, `yarn install --immutable`, and clean lockfile diff after install.
- The npm package must still include the intended NAPI loader, type declarations and platform package metadata — inspect `npm pack --dry-run --json`.
- All workflow path filters must cover each ecosystem's manifests, lockfiles and source — validate workflow YAML and test path lists.
- CI/publish jobs must use least-privilege permissions and pinned third-party actions without changing release-unit behavior — inspect workflow permissions and release conditions.

---

### Task 1: Establish one repository boundary and document local workflows

**Files:**

- Modify: `.gitignore`
- Modify: `Cargo.toml` only if needed to keep the workspace lockfile explicitly trackable
- Remove: `crates/taunicorn-javascript/.git` metadata only (preserve every source/package file)
- Create: `CONTRIBUTING.md`

**Interfaces:**

- Produces: documented local verification commands for Cargo, uv/Python, Yarn/Node and package inspection.

- [x] Re-check the nested Git directory and confirm it has no commits or submodule registration; record the root's current status before removing only that metadata.
- [x] Update ignore rules for local Python/Rust/Node build artifacts; ensure root `Cargo.lock`, `uv.lock`, and Yarn lock remain visible to Git.
- [x] Remove the nested `.git` metadata without changing `crates/taunicorn-javascript/` source files.
- [x] Add concise `CONTRIBUTING.md` sections for repository map, required toolchains, local checks and release ownership.
- [x] Verify `git -C crates/taunicorn-javascript rev-parse --show-toplevel` resolves to the root; verify root lockfiles are not ignored and generated local binaries are ignored; compare `git status --short` with the pre-change baseline.

### Task 2: Make Yarn 4 canonical and preserve npm distribution

**Files:**

- Modify: `yarn.lock` (regenerate with the vendored Yarn 4 executable; never hand-edit)
- Remove: `package-lock.json` only after immutable Yarn resolution passes
- Modify: `package.json` for the explicit ESM/CJS package boundary and Yarn manager consistency
- Modify: `__test__/index.spec.ts` to exercise the real public local duplex API
- Modify: `.github/workflows/publish-javascript.yml` for manager-consistent test/install commands
- Modify: `.github/workflows/security.yml` and `.github/dependabot.yml` for npm dependency coverage

**Interfaces:**

- Consumes: pinned Yarn 4.18.1 at `.yarn/releases/yarn-4.18.1.cjs`.
- Produces: one authoritative Yarn lock and reproducible npmjs package validation.

- [x] Use the local Yarn 4 executable to migrate/regenerate the existing lock and resolve dependencies; do not invoke global Yarn or npm install.
- [x] Run Yarn immutable install; only then remove the redundant npm lockfile.
- [x] Ensure all development/CI test commands use Yarn, while release continues to publish npm-compatible packages to npmjs.org with the existing napi-rs prebuilt-binary model.
- [x] Add npm ecosystem coverage to Dependabot and a Yarn npm audit job/step for direct and transitive dependencies.
- [x] Verify `yarn install --immutable`, `yarn lint`, `yarn test`, and `yarn npm audit --all --recursive`; run `npm pack --dry-run --json` and confirm expected loader/types/package files are present.

### Task 3: Close Cargo/uv workspace and package-boundary gaps

**Files:**

- Modify if justified: `Cargo.toml`, `crates/taunicorn-javascript/Cargo.toml`, `pyproject.toml`
- Modify: `.github/workflows/rust-ci.yml`, `.github/workflows/python-ci.yml`

**Interfaces:**

- Consumes: current Cargo workspace, Maturin/PyO3 project and `uv.lock`.
- Produces: explicit package-publish boundaries and CI checks that honor committed locks.

- [ ] Inspect `cargo metadata --locked` and package metadata; mark the JavaScript binding crate non-publishable if it is not intended for crates.io, matching the existing Python binding crate boundary.
- [ ] Keep language package versions independent; do not centralize versions or change release tags.
- [ ] Ensure Rust CI checks/tests the workspace with `--locked`, and Python CI uses uv's locked project/requirements operations where applicable.
- [ ] Verify `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `uv lock --check`, Python tests, and the existing wheel/sdist build path.

### Task 4: Harden and align GitHub automation

**Files:**

- Modify: `.github/workflows/rust-ci.yml`
- Modify: `.github/workflows/python-ci.yml`
- Modify: `.github/workflows/publish-rust.yml`
- Modify: `.github/workflows/publish-python.yml`
- Modify: `.github/workflows/publish-javascript.yml`
- Modify: `.github/workflows/security.yml`
- Modify: `.github/dependabot.yml`

**Interfaces:**

- Consumes: independent crates.io, PyPI and npmjs release flows.
- Produces: least-privilege CI and release workflows with verified immutable action references.

- [ ] Audit each workflow's event/path filters, permissions, credentials and release conditions before changing it.
- [ ] Set read-only defaults and elevate permissions only on publishing jobs; disable persisted checkout credentials when not needed.
- [ ] Pin third-party actions to verified full commit SHAs with version comments; configure Dependabot to keep action pins updated.
- [ ] Keep package-specific release triggers, OIDC/trusted-publishing boundaries and artifact matrix semantics unchanged.
- [ ] Validate workflow syntax with `actionlint` if available (otherwise use an available YAML/workflow validator and report the limitation); verify every external action reference is SHA-pinned and each publish job has only required permissions.

### Task 5: Integrated regression and worktree verification

**Files:**

- No additional source changes unless a prior task's check exposes a defect.

- [ ] Run `git diff --check` and review the final diff against the initial dirty-worktree snapshot; confirm no user file was reset, staged or unintentionally overwritten.
- [ ] Run the applicable Cargo, uv/Python and Yarn/Node checks from Tasks 2–4; record platform-only CI checks that cannot run on this Windows host.
- [ ] Confirm `npm pack --dry-run` succeeds, lockfiles remain immutable, and the three publishing workflows still version/release independently.
- [ ] Report any existing vulnerability audit findings or unavailable checks explicitly; do not suppress advisories or claim legal certification.
