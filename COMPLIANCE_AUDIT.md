# 🛡️ Taunicorn | IPC Security & EU Compliance Audit

> **Audit date:** 22 November 2026  
> **Status:** 🟠 Technical hardening implemented in the source tree · Operational acceptance pending  
> **Scope:** Local duplex IPC · Rust, Python and Node.js · EU regulatory alignment  
> **Disclaimer:** This document is **not** a release approval, regulatory certification, or legal advice.

**Historical baseline:** A locally modified working tree on `main`, HEAD `0c9b49583f72f2b23415a1ea8cbff3cebf1594e7`. Findings do **not** automatically apply to published releases.

---

## 🔎 Executive Review | `main` (`7c6ea65`), 22 November 2026

### 🔐 Transport and platform controls

Rust uses Unix domain socket (UDS) streams and Windows duplex named pipes without TCP/IP; Python and Node.js use the same core. `interprocess` is pinned to **2.4.8**. Private Unix endpoints verify the runtime directory and kernel credentials; private Windows endpoints check the pipe DACL and security token. Remote-client rejection and the first-instance flag are established by inspection of the pinned Windows backend, **not** by SMB testing. `GenericNamespaced` remains enabled for backward-compatible entry points in the default build, without equivalent peer verification; `compliance-profile` routes these entry points to the private implementation. Reads and writes have separate locks. Local write shutdown does not prove peer EOF. No latency or zero-copy guarantee follows from the architecture. ([1], [2], [5], [9], [11])

### 🚧 Admission-boundary limitations

The private profile requires the **same operating-system account**; optional UID/GID/PID/SID filters may only narrow access. Owner-only permissions comparable to `0600` are intentional; `0660` group access is unsupported. If a required PID is unavailable on the target OS, admission fails. The cap of **64 concurrent admissions** and connection budgets apply **per core instance**, not per PID or across the host. The five-second timeout cannot interrupt synchronous OS calls during admission: latency and saturation behavior must be measured on the target OS. `AdmissionError` is typed in Rust but is not a stable Python or Node.js API error code. Expose it additively only for a specific binding consumer; do not classify platform errors as remote rejection or name collision without unambiguous OS evidence. ([1], [2], [5])

### 🧩 Framing, privacy and governance

`security.rs` validates handshake-frame lengths **before allocation**. The optional Rust wrapper `profile::Session` limits application frames to **512 plaintext bytes** and filters its typed status schema before encryption and after decryption. The raw byte API does **not** filter personally identifiable information (PII). Audit submission via `try_send` does not wait for storage, is not a durable acknowledgment, and misses OS-level rejection before `accept`. Add other schemas only through explicitly versioned wrappers. For mandatory audit logging, combine the queue and OS audit with externally protected storage. Portability, key operations, and trust services need separate acceptance; the existing key pin does not establish certificate validation or eIDAS compliance. ([1], [2], [5], [6], [7], [8])

### 📦 Supply chain

A fresh OSV batch query of the **106 unique crates.io name/version pairs** from `Cargo.lock` returned no findings. In Python push run `44974028480`, RustSec, recursive Yarn audit, Python runtime-dependency audit, and Gitleaks passed. This excludes some Python build/test dependencies and untracked files. `scripts/ci/release_sbom.py` binds the software bill of materials (SBOM) to the packaged artifact's SHA-256; resolved lockfiles and hashes do not substitute for linkage verification or provenance. ([5], [14])

### ✅ Verification and remaining approvals

On Windows, **27 Rust transport-crate tests passed per build**, both default and `compliance-profile`. GitHub push runs `44974028476` (Rust), `44974028480` (Python) and `44974028467` (JavaScript) passed on `7c6ea65` using staging, **without publishing**. They do not verify the locally modified `needs` ordering. Separate OS accounts, SMB rejection, production audit export, operational approval, and legal review remain outstanding.

### ➡️ Migration path from this revision

1. **Move both ends together:** Switch both endpoints of each protected service to private entry points; test admitted and rejected accounts on the target OS without falling back to legacy paths (GDPR Arts. 25/32; NIS2 Art. 21; CRA Annex I). [1][2][5]
2. **Extend only the message layer:** Add the required versioned schema and validate filtering, reconnection, p99 and throughput against the unchanged duplex API. Open formats support interoperability where Data Act Arts. 30/33–36 apply. [6]
3. **Keep trust services separate:** Assess certificate hooks only for a specific trust service. Key residency and trust verification are not controlled by the IPC library (eIDAS Arts. 5a/25–26, where applicable). [7][8]
4. **Close operational gaps before rollout:** Connect OS audit and the exporter to protected storage; rehearse alarms and restoration; assess reporting obligations separately (GDPR Art. 33; NIS2 Art. 23 / German BSIG § 32; CRA Art. 14). [1][2][4][5]

---

## 🧪 Earlier Reassessment | 22 November 2026

**Test baseline:** `feat/taunicorn-benchmark-ci`, base `d0f852a`; `interprocess` 2.4.8 according to `cargo tree --locked`. This inventory supplements earlier findings but does **not** constitute release approval. OS and protocol evaluations cover local duplex IPC only.

| Area | Confirmed in the examined source | Acceptance still required |
|:--|:--|:--|
| **Unix transport & peer admission** | `compliance/unix.rs` validates `XDG_RUNTIME_DIR` and ancestors using descriptors (`openat`, `O_NOFOLLOW`); no fallback to `/tmp` for private endpoints. Linux binds via `/proc/self/fd`; Linux/OpenBSD request `0600`. macOS rejects extended directory ACLs; FreeBSD rejects filesystems with NFSv4 ACLs. macOS/FreeBSD may need `umask 0077` on startup so the socket remains owner-accessible only. `compliance.rs` requires the same OS account and optionally checks UID/GID/PID; missing required attributes cause rejection. | Test real distinct accounts. This backend does not expose a PID on macOS. On ARM64 the test setup failed on the runner home directory's `group:everyone deny delete` ACL before admission (Actions `44963851869`). |
| **Windows transport & peer admission** | `compliance/windows.rs` configures an owner DACL, checks the connected pipe handle on the client, and checks the server-side token after a bounded opening byte. The pinned `interprocess` implementation statically establishes `PIPE_REJECT_REMOTE_CLIENTS` as a default and `FILE_FLAG_FIRST_PIPE_INSTANCE` for the first instance. PID filtering uses OS snapshots, not a service identity. | Distinct accounts and SMB remote rejection on the target OS, including OS logging for rejection before `accept`. |
| **Legacy paths without the profile** | `lib.rs` uses `GenericNamespaced`: abstract UDS on Linux, filesystem endpoints under `/tmp` on macOS/BSD, and local duplex pipes with OS-default permissions unless an explicit DACL is configured. Without the profile or explicit private entry points, there is no shared peer gate. | Do not approve legacy paths for protected services. Switch and test both listener and connector against the same hardened profile before deployment. |
| **Duplex, framing & identity** | `lib.rs` separates read/write directions. Rust, Python and Node delegate to one Rust core; private entry points coexist with backward-compatible raw entry points when the Cargo feature is absent. `security.rs` bounds handshake/frame lengths before allocation; Secure v2 authenticates pinned keys. `profile.rs` filters only the typed Rust status schema in both directions—no general PII filter on raw bytes and no automatic certificate or wallet validation. | Compare production load and latency to baseline; verify language roles and protocol compatibility. Zero-copy and kernel bypass are not established. |
| **Resource limits** | `compliance.rs`: up to 64 concurrent OS admissions, five-second admission deadline, default maximum 256 total connection endpoints and 64 per account within the process. `security.rs` additionally imposes a 10-second Secure-v2 handshake deadline. The status profile limits frame sizes, directional budgets and idle time. | No per-PID quota, cross-process budget or heartbeat. Measure simultaneous saturation, memory and p99 before adding more limits. |
| **Privacy, audit & supply chain** | `profile/audit.rs` provides a bounded queue of fixed events. Independently, CI found a payload leak in the Python NDJSON example and four rejections of embedded wheel SBOMs with `pyo3 0.29.3` (Actions `44962661739`). | Independently verify payload-free examples, archive reconciliation across the host matrix, durable audit anchoring and a documented reporting process. |

### 🛠️ CI corrections

Rust and binding tests on macOS receive a dedicated private directory under `/Users`, without modifying the runner-home ACL or library validation rules. On Windows, **27 Rust tests passed per build** (with/without profile), along with Clippy, **seven Python tests** (one Unix skip), **seven Node tests** against a newly built add-on, and **two NDJSON tests** covering final response, EOF and absence of payloads in example logs. On GitHub Actions, macOS binding tests must actually perform private data exchange; `UnsupportedPlatform` must not count as a pass. This did not yet establish a fully green pipeline for registry publishing.

### 🔍 Dependencies and secrets

In GitHub Actions run `44963851869` on **21 November 2026 (UTC)**, commit `d0f852a`, RustSec (`Cargo.lock`, 109 dependencies), recursive Yarn audit, and the pinned Python runtime-dependency audit passed. Gitleaks reported no findings across **46 scanned commits**. Coverage excludes untracked files, current local changes, and some Python build/test dependencies.

### 📄 SBOM finding

All four wheel jobs inherited `CARGO_TERM_COLOR=always`. ANSI control sequences surrounding the duplicate marker `(*)` in `cargo tree` entered the compared license value. The same defect was reproduced with a local Windows wheel built using Maturin 1.14.1. Using `cargo tree --color never` resolves the comparison even when color remains enforced in the environment. Version, license, and SHA-256 checks remain mandatory.

### 🔄 GitHub follow-up for `5ae96a5`

Rust transport tests passed on macOS Intel and ARM64 (`44972150705`). Python PR run `44972150706` passed all four wheels and the sdist, including SBOM and installation tests; nothing was published. In JavaScript run `44972150733`, the AVA test against the packaged Linux add-on passed; only the formatter check for `benchmark/ordinary-duplex.mjs` failed. The file was formatted locally without changing IPC or benchmark logic. Operational testing with distinct accounts and Windows SMB remains outstanding.

### 📋 Next steps & acceptance

1. **Private endpoints first.** Require the existing `IpcCompliancePolicyEngine` for protected services, without silently reinterpreting legacy names or raw bytes. Test OS admission with distinct accounts on each supported platform; deny a stream if required credentials are unavailable. A PID allowlist is not usable on macOS with the current backend. Quotas apply per account, not per PID. (GDPR Arts. 25/32; NIS2 Art. 21; CRA Annex I.) [1][2][5]
2. **Filter messages, not raw bytes.** Reuse `profile::Session` for further application data only through explicitly versioned schemas. Keep the existing byte API and Secure-v2 wire format unchanged. Before approving a new schema, demonstrate independent budgets, bidirectional filtering, concurrent duplex progress, throughput and p99 against baseline. Add adapters or certificate hooks only for a specific consumer or trust service; key management and trust anchors remain operator responsibilities. (GDPR Arts. 25/32; NIS2 Art. 21; CRA Annex I; Data Act Arts. 23–30/33–36; eIDAS Arts. 5a/25–26, as applicable.) [1][2][5][6][7][8]
3. **Approve operations and release independently.** Connect the audit queue to protected externally anchored storage and supplement it with OS audit to capture rejections before `accept`. The operator must test retention, recovery and incident reporting. Release only after the actual archives pass SBOM reconciliation across all target platforms. (GDPR Arts. 5/33; NIS2 Art. 23 / BSIG § 32; CRA Art. 14 and Annex I.) [1][2][4][5]

---

## 🧭 Earlier Post-Implementation Review

The following table and Sections 1–5 record **older checkpoints**. Where results differ, the dated reassessment above takes precedence. Published builds without corresponding features do not automatically share these properties.

| Audit ID | Implemented safeguard / verified path | Remaining acceptance |
|:--|:--|:--|
| **01 · Peer admission** | `compliance.rs`, `compliance/{unix,windows}.rs`: same-user checks at both ends, with optional UID/GID/PID/SID restrictions. Rust/Python: `listen_private` / `connect_private`; Node: `listenPrivate` / `connectPrivate`. With `taunicorn/compliance-profile`, existing transport entry points also go through the gate. Windows: restrictive pipe DACL, client handle verification and server identity token; Unix: directory, socket and kernel-credential checks. | Real distinct accounts, Windows SMB and native macOS/BSD runs. Unix socket names intentionally remain after shutdown; no unlink of foreign files. An admitted account does not identify a particular service. |
| **02 · Capacity** | `compliance.rs`, `security.rs`: bounds on active admissions/connections; handshake limit 64 and total deadline 10 s; exact Hello/Finished lengths checked before allocation. The profile limits data/time budgets per direction; bindings bound buffers per raw read call. | Before/after benchmarking under identical conditions and saturation of all slots are not established as production load tests. |
| **03 · Secure v2** | `security.rs`, `tests/secure_limits.rs`: size validation before encryption, reserved sequence numbers; cancellation during an active operation terminates the session and wakes waiters. Cancelling a merely waiting call does not destroy the session. `profile::Session` exposes no raw handle. | Legacy access remains in the existing Secure-v2 API. Treat only the explicit message profile as a strict boundary; do not claim a new cryptographic audit. |
| **04 · Half-close** | `tests/duplex_lifecycle.rs`, `scripts/ci/test_example_close.py`, examples and README: local write shutdown, reverse-direction traffic and EOF after full close are tested separately. No hidden control bytes in the raw stream. | Native macOS/BSD acceptance; Windows reference measurements in `benchmark/ordinary-windows.md`. |
| **05 · PII & audit** | `profile.rs`, `profile/audit.rs`, `tests/profile.rs`: typed Rust status channel with bidirectional field allowlist; a synthetic private note does not reach consumers. Bounded audit channel, fixed events, observable drops and refusal of profile admission on full/disconnected queue. | Not universal filtering: Python/Node expose private raw streams but no public status-profile facade. Queue admission is not a persistent acknowledgment. External integrity anchoring, alarms, rotation, retention and OS pre-admission rejections belong to the operator. |
| **06 · Supply chain** | `release_sbom.py`, `test_release_sbom.py`, publish workflows: separate CycloneDX attestations per archive with SHA-256 and commit; wheel inventory reconciled. Yarn lockfile fixed; unrestricted recursive audit has no findings. | Host matrix and actual publication/archival have not run. Dependency resolution does not prove full static binary linkage. |

### ✅ Final checks in the shared working tree

On Windows, `cargo test --locked --offline --workspace --all-targets --features taunicorn/compliance-profile`: **27 passed** (9 unit, 3 lifecycle, 10 profile, 4 secure-limits, 1 shared-API). Clippy for the same workspace with `-D warnings` passed; `cargo fmt --all -- --check` still failed. A previously recorded temporary lockfile error was fixed. Two SBOM unit tests and workflow lint passed. Separate runs: Linux WSL core **27 passed**; macOS ARM64 / FreeBSD x86-64 had compile checks only. Windows bindings: Python **7 passed / 1 Unix skip**; Node **7 passed**; private Python↔Node connectivity worked in both roles. No production identity, durable log storage or legal approval was simulated. Operator checklist: `docs/COMPLIANCE_OPERATIONS.md`; schema and boundaries: `docs/IPC_PROFILE.md`.

### ⚠️ Explicit profile boundaries

Default builds retain legacy transport paths. The Cargo feature enforces OS admission, **not** schema filtering of every raw byte. The implemented policy further restricts access within the same account; it is **not** cross-account `0660` group access. Mappings to GDPR/NIS2/CRA/Data Act/eIDAS in Section 3 describe technical contributions, **not** legal approval. Pinned keys and external trust providers are not integrated X.509 or wallet validation. The library does not enforce private-key storage location or an EU region. A rejected connection before profile-session creation does not automatically appear in the profile audit exporter. Zero-copy, kernel bypass and unchanged production latency have not been demonstrated.

---
## 1. 🏗️ Architecture | What the Project Does and Where Trust Begins

Taunicorn connects local processes using ordered, asynchronous duplex byte streams. Rust/Tokio owns listeners and connection state; Python/PyO3 and Node.js/napi-rs delegate to that same core. The assessed scenario involves multiple local processes/users with different privileges and concurrent sessions, **not TCP/IP transport**. A local peer is not inherently authorized; even after a UID/SID check, a process belonging to the same user is not necessarily the expected service.

| Path | Observed baseline | Security implication |
|:--|:--|:--|
| **Listener / Connector** | `crates/taunicorn/src/lib.rs:119–126,174–248,781–817`: name resolution, `create_tokio`, `accept`, `RawStream::connect`. Optional Unix mode or Windows SDDL during `bind`; `start` does not set its own access policy. | No common authorization gate before passing a stream to the application. `ConnectionInfo` carries endpoint/status, not OS-verified identity. |
| **Raw stream** | `crates/taunicorn/src/lib.rs:375–393,471–611,713–768`: independent directional locks, socket I/O and cancellation. `receive` returns arbitrary chunks. | No message or PII boundary; successful `send` does not acknowledge processing by the peer. |
| **Secure v2** | `crates/taunicorn/src/security.rs:136–263,291–413,420–525,742–810`: mutually pinned Ed25519 identities, ephemeral X25519, HKDF-SHA256, direction-separated ChaCha20-Poly1305, sequence validation, length framing and explicit key confirmation. | Optional message layer, not kernel identity checking or a qualified trust service. Preserve the codec/wire contract independently of the raw stream. |
| **Bindings** | `crates/taunicorn-python/src/lib.rs:585–600,675–688`, `crates/taunicorn-python/src/security.rs:51–86,163–219,255–314`; `crates/taunicorn-javascript/src/secure.rs:121–214,263–297,423–454`. | Python protects upgrade transitions and terminates cancelled secure I/O; Node locks sessions on errors/cancellation and enforces a handshake timeout. These rules are not consistently rooted in the Rust core. |

### 1.1 API snapshot at the time of inventory

Rust, Python and Node.js offer `Server.listen` and `Client.connect` as shared asynchronous entry points. They use the same raw transport and do not perform additional peer identity checks. At this historical checkpoint, the hardening planned for Step 2 had **not yet been implemented**.

### 1.2 Platform findings | Pinned-dependency source inspection

For CI/CD, `cargo tree -p taunicorn --locked -i interprocess` continues to confirm **2.4.8**. In the then-current `crates/taunicorn/src/lib.rs`, `listen` (175–179) leads to `bind` (187–215); `accept` (232–248) and `connect` (799–813) return streams without an additional peer policy. The line numbers in the original inventory below are historical anchors. The Rust integration test `crates/taunicorn/tests/shared_api.rs` has since been added; the original statement of “0 Rust tests” is not the present test count. A passing IPC smoke test demonstrates neither access control nor regulatory compliance.

| Platform | Existing controls at that checkpoint | Missing proof / next action |
|:--|:--|:--|
| **Linux** | `GenericNamespaced` uses abstract UDS rather than a socket file; `0600` file permissions therefore cannot enforce access. `interprocess` exposes `peer_creds()` based on `SO_PEERCRED`, but Taunicorn did not call it. [9] | For hardened endpoints use an explicit filesystem path in a verified private runtime directory; `0600` for owner access or `0660` with an explicitly authorized group and suitable directory permissions. Verify kernel UID/GID at both ends and any explicitly required PID. PID is not a persistent identity proof. No silent fallback to the abstract namespace. [9][19] |
| **macOS / BSD** | `GenericNamespaced` falls back to filesystem UDS under `/tmp`; Taunicorn did not verify an application-owned parent directory. Available credential information is OS-dependent; the pinned dependency uses, for example, `LOCAL_PEERCRED` on macOS and `LOCAL_PEEREID` on NetBSD. | Test UID/GID availability and socket/directory permissions per target OS. If policy requires a PID that cannot be reliably obtained, reject the connection or mark the platform unsupported; never silently weaken the policy. |
| **Windows** | Local duplex byte pipes. **Correction to the initial finding:** `PIPE_REJECT_REMOTE_CLIENTS` and `FILE_FLAG_FIRST_PIPE_INSTANCE` exist in the pinned backend (remote rejection by default; first-instance flag only for the first handle). Without SDDL, Taunicorn did not set its own restrictive DACL. Windows `peer_creds()` only yields a process ID, not a verified user token. [11][12] | Grant minimum necessary rights to approved SIDs; do not copy a permissive `Everyone` DACL from examples. Verify client and server PIDs according to connection direction; check the client token on the server within synchronous impersonation and revert before each `await`. Because Windows impersonation uses the security context of the last pipe message read, restrict any necessary initial read to a small, timed protocol greeting. Release no application data beforehand. PID alone is not authorization. DACL/remote rejection may occur before `accept` and is not guaranteed as a library audit event. [11][12][13] |

**Pinned `interprocess` 2.4.8 source anchors:** `src/local_socket/name/type.rs:94–112`, `src/os/unix/local_socket/peer_creds.rs:14–67,194–219`, `src/os/windows/named_pipe/local_socket/tokio/listener.rs:13–32`, `src/os/windows/named_pipe/listener/options.rs:81–94`, `src/os/windows/named_pipe/tokio/listener.rs:85–119`, `src/os/windows/named_pipe/listener/create_instance.rs:77–105`, `src/os/windows/named_pipe/local_socket/tokio/stream.rs:61–67`. The flags are **verified statically**, not by SMB or multiuser tests. First-instance creation detects a name collision; it does not prove listener identity to the client. [11]

---

## 2. 🚨 Prioritized Security Gaps | Historical Inventory

> **Reading note:** The P0/P1 findings below describe the **earlier baseline**. Later implementation and reassessment results at the top of this document take precedence.

| ID / priority | Original finding | Minimum meaningful fix / consequence if left unfixed |
|:--|:--|:--|
| **01 · P0 — Missing shared OS authorization** | `accept` / `connect` returned streams without a credential policy (`lib.rs:227–242,781–807`). Default Linux endpoints had no filesystem ACL; macOS/BSD used a shared temporary namespace; Windows relied on default permissions if no SDDL was configured. An overly permissive `Everyone` SDDL appeared in the JS API documentation (`crates/taunicorn-javascript/src/lib.rs:83–86`). | Private endpoints, least-privilege ACLs and a policy gate on both sides **before** exposing the stream. Otherwise restriction depends on the integrating application; source code alone does not prove unauthorized use actually occurred. |
| **02 · P0 — Inadequate pre-authentication resource bounds** | Fixed Hello was checked only *after* a generic framed read (`security.rs:198,437,620–655,742–786`), which could allocate up to the general **64 MiB** limit. Node had a handshake timeout but the core lacked a consistent deadline and session budgets. Raw streams had no message limit; bindings could allocate read buffers according to API parameters. | Validate context-specific size before allocating; enforce fixed handshake lengths, a short total deadline, global and per-identity limits for handshakes/connections, byte budgets and idle deadlines. The **existing** 64 MiB limit was insufficient for the pre-authentication path. No TCP/IP or background protocol is needed. |
| **03 · P0 — Inconsistent Secure-v2 failure semantics** | Rust did not consistently make cancellation during partially progressed frame operations terminal; send sequence state was updated only after I/O success (`security.rs:296–350,374–404`; `lib.rs:562–611`). Python/Node added their own safeguards. Rust also exposed raw access through `SecureConnection::connection()` (`security.rs:279–285`). | Validate size before encryption; atomically disable the session after uncertain progress, framing/AEAD failure or cancellation, waking waiters too. Do not retry automatically and do not expose writable raw access in hardened mode. This was a static finding; no cryptographic exploitation was tested. |
| **04 · P1 — Half-close does not signal peer EOF in generic backend** | Taunicorn called `shutdown()` (`lib.rs:713–734`); the relevant `interprocess` implementation was a no-op (`src/local_socket/tokio/stream/enum.rs:36–60`). On Windows, local `shutdownWrite` left the peer's read open during the test window while reverse traffic still worked; EOF appeared only after full close. A Python example expected peer EOF (`examples/python/js_server.py:59`). | Distinguish local shutdown from peer EOF in the public contract and verify all target platforms. Use native OS half-close when demonstrated; otherwise use an explicit end-of-message frame only in an agreed message protocol. Do not insert new control bytes into raw streams. |
| **05 · P1 — No bidirectional PII policy or structured audit** | Raw bytes have no reliable data type; directional gates implemented no schema policy. Diagnostics/error strings do not constitute an auditable incident record. | Attach PII rules only to typed messages and a testable schema. Filter outbound after application validation and before encryption; inbound after authentication/decryption and before delivery. Reject uninspectable content in strictly configured channels. By default, logs must exclude payloads, names, paths, keys and raw user identifiers. Never market an unprotected raw stream as PII-filtered. |
| **06 · P1 — Partial supply-chain and test evidence** | `LICENSE.spdx:10–17,50–54` was a baseline inventory, not a complete artifact-specific SBOM. Existing Node CI audit used only `--severity high`; an unrestricted scan identified two moderate transitive advisories (see below). The baseline Rust test command compiled but executed **zero tests**. | Update affected Node build dependencies or track justified, time-bounded exceptions—no silent suppression. Inventory resolved components/licenses for each `.crate`, wheel and npm platform package, including bundled native libraries where applicable. Validate wheel SBOM contents; bind release hashes and commits. Add platform, negative-path and interoperability tests to CI. |

**Lower-priority finding:** `SECURITY.md:47–63` covered only the raw layer and did not completely describe supported bindings. During migration, distinguish raw stream, Secure v2 and hardened profile. No hardcoded production IPC token was found in the examined handshake paths; fixed magic values, labels and example endpoint names are not secrets. No full secret scan was performed; `.env` and credential files were not read.

---

## 3. ⚖️ EU Regulatory Mapping | Technical Measures Are Not Compliance Proof

| Framework | Applicability and technical relevance | Acceptance / responsibility |
|:--|:--|:--|
| **GDPR**, Art. 5(1)(c)/(e), Arts. 25, 32 and 33 | When personal data is processed: data minimization and storage limitation, risk-appropriate security, data protection by design and by default. Peer policy, schema filtering, PII-minimal logs, key protection and availability testing support these goals. Art. 33 creates a separate breach-notification path (generally, where notification is required, without undue delay and where feasible within **72 hours** of awareness—not after every IPC rejection). The articles do not mandate particular socket flags or universal redaction. [1] | The controller determines purposes, data classes, risks, retention and reporting; processors inform controllers without undue delay. Do not equate pseudonymization with anonymization. [1] |
| **NIS2**, Arts. 20, 21, 23; **Germany: BSIG §§ 30, 32** | For covered entities: risk-based cybersecurity governance, incident handling, supply-chain security and access control. For significant incidents, generally an early warning within **24 hours** and an incident notification within **72 hours** of awareness; a final report generally within one month after notification, subject to specific provisions (including trust services and ongoing incidents). [2][3][4][20] | Operators classify incident significance and notify the competent recipient; the library can supply protected evidence, not automatic legal determinations or authority notifications. |
| **Cyber Resilience Act (CRA)**, Arts. 2, 3, 13, 14, 24, 71; Annex I Parts I/II | Applicability depends on EU-market placement and the actor's role. Free/open-source software supplied outside commercial activity is exempt, with a separate regime for open-source software stewards. Secure defaults, resource resilience, vulnerability handling and a machine-readable SBOM containing at least direct dependencies support manufacturer obligations. **Art. 14 applies from 11 September 2026**; the CRA generally applies from **11 December 2027**, including steward reporting duties under Art. 24(3). Art. 14 covers actively exploited vulnerabilities and severe security incidents and is separate from NIS2/GDPR reporting. [5][18] | Establish manufacturer/steward status, delivery model, support period, SBOM, vulnerability management and reporting readiness before a covered release. |
| **Data Act**, Arts. 1, 23–30, 33–36 | Switching obligations for data processing services and related interoperability rules depend on the service and the actor's role. A local IPC library does not fall under them solely because it defines a wire format. Documented portable framing and interoperability tests are useful technical contributions; the Act does not impose a blanket E2EE or EU key-hosting requirement for local sockets. [6] | Provider and user determine the use case; publish a format description and reciprocal tests rather than adding adapters without an actual consumer. |
| **eIDAS, as amended by Regulation (EU) 2024/1183**, Arts. 5a, 25–26, 35–36 | A pinned process key, local signature or ordinary certificate is neither an EU wallet identity nor a qualified electronic signature/seal. An optional hook is only an integration point for suitable trust chains and the relevant use case. [7][8][17] | Operators/trust-service providers validate trust anchors, validity, revocation, identity binding and regulatory qualification where necessary. The library does not claim qualified status. |

---

## 4. 🗺️ Minimum Migration Plan & Acceptance Criteria

Operator checklist and ownership: `docs/COMPLIANCE_OPERATIONS.md`. The items below are **requirements**, not attestations. Source inspection, local platform tests and acceptance in the deployed environment are separate forms of evidence. Historical measurements below do not approve the hardened profile.

### 4.1 🔐 Connection-boundary policy gate

**Regulatory mapping:** GDPR Arts. 25/32; NIS2 Art. 21; CRA Annex I.

Implement one `IpcCompliancePolicyEngine` in the Rust core, once per handshake, at the existing `Server::accept` and `Connection::connect` transitions of the **new hardened entry points**. Platform adapters obtain snapshot credentials and create secure endpoints. Do not recheck every byte chunk.

- **Unix:** controlled endpoint names only; locally verified runtime directory; no symlink traversal or blind removal of another owner's socket files; check ownership/mode and reject collisions safely; no silent fallback.
- **Windows:** explicit DACL; regression-test pinned backend flags; use token/PID checks only when reliably bound to the handle.
- **Errors:** typed failures such as `UnauthorizedIpcPeer`, `InsecureSocketPermissions`, `StreamQuotaExceeded`. Report `RemotePipeAccessDenied` or `PipeSquattingDetected` only when OS evidence genuinely distinguishes these states.

**Acceptance:** authorized and unauthorized peers, unavailable credentials, insecure parent directory, name collision, DACL/remote rejection on each applicable platform. **No unverified stream reaches the application.**

### 4.2 🧩 Message wrapper over existing duplex transport

**Regulatory mapping:** GDPR Arts. 25/32; NIS2 Art. 21; CRA Annex I; Data Act where relevant.

Do not introduce implicit framing in `Connection`; reuse Secure v2. Enforce small limits before allocation, independent directional budgets, per-identity capacity plus a global cap, and release slots on failure/close. Apply policies before encryption or after decryption **to typed messages**, not arbitrary byte chunks. Start with an idle deadline; add heartbeats only where evidence shows a need and within a mutually agreed message protocol. Do not reimplement cryptography. Operators define key storage, rotation and trust anchors. Document framing separately; add codec or certificate-validation hooks only for concrete demand.

**Acceptance:** valid bidirectional data; fragmented and coalesced valid frames; boundaries ±1; invalid authentication; cancellation without session reuse; independent directional progress; quota release; reconnection; legacy interoperability.

### 4.3 🧾 Bounded audit side channel

**Regulatory mapping:** GDPR Arts. 5/25/32; NIS2 Art. 23 / BSIG § 32; CRA Art. 14.

Use fixed events and reason codes, UTC timestamps plus monotonic ordering, minimal correlation and **no payloads**. An asynchronous sink should have a bounded queue, backlog/drop counters and alarms—no silent loss. When decisions require mandatory recording, decide on fail-closed behavior **at the connection boundary** rather than blocking both I/O directions on logging. Use an integrity proof plus a protected, externally anchored append-only sink for tamper detection; a local hash chain alone is not tamper-proof. Correlate separate OS audit if the OS rejects remote/ACL access before handing it to the library.

**Acceptance:** a synthetic PII canary appears in neither allowed events nor error logs; rotation, retention and export are tested; deliberately disconnecting the sink creates an observable failure; the export does not itself send regulatory notifications.

### 4.4 🚀 Rollout without breaking the protocol

Mark legacy entry points clearly as **not hardened**, rather than silently changing behavior. Explicitly activate new secure service endpoints. Production rollout requires every listener and connector running under the compliance profile to enforce the gate. Test Linux, macOS/BSD and Windows with distinct accounts; test Rust/Python/Node in both roles. Compare throughput, p50/p99 and memory with baseline during concurrent duplex load; retain bounded queues and independent read/write locks. Zero-copy is not established; avoid adding copies across existing language/crypto boundaries.

**Release acceptance:** document risk decisions, platform matrix, per-artifact SBOM and incident-response procedures as evidence.

---

## 5. 📊 Executed Checks & Remaining Limits

### 5.1 Duplex baseline — **not** a hardened-profile measurement

`benchmark/ordinary-windows.md` records three Windows runs against the existing Node add-on (SHA-256 in the measurement log), with successful reconnection.

| Metric | Observed range | Interpretation |
|:--|--:|:--|
| **64-byte echo p50** | **2.643–3.220 ms** | Baseline only |
| **64-byte echo p99** | **12.460–43.388 ms** | Baseline only |
| **Aggregate duplex throughput** | **5.717–6.943 MiB/s** | Baseline only |

These figures are neither guaranteed limits nor regression evidence for later builds. Three tests in `crates/taunicorn/tests/duplex_lifecycle.rs` ran against a cached Windows build. During separate work-in-progress testing, `cargo test --locked -p taunicorn --test duplex_lifecycle` rejected a not-yet-updated lockfile (exit **101**); this does not establish test results for the new source. The Windows test `scripts/ci/test_example_close.py`, covering a synthetic identifier, final reply and full close, passed against the installed Python package. Other OSes and separate accounts are not covered by that test.

### 5.2 CI/CD working-tree reassessment

On Windows, `cargo test -p taunicorn --test shared_api --locked` ran **1 passing test**. A Python↔Node interprocess test and three artifact-integrity unit tests also passed. An independent local security scan found no issues in the scanned Git history (**42 commits**, excluding untracked worktree files), RustSec, inspected Python runtime export, or Yarn at severity `high` or above. These results do not replace the inventory below, which used different coverage. In particular, a Yarn `high` threshold does not mean no moderate advisories exist. `cargo fmt --all -- --check` failed in eight pre-existing Rust source files and remained a release blocker. No hosted or registry test was established.

### 5.3 Original baseline before API/pipeline changes

| Check | Result on local Windows host |
|:--|:--|
| `cargo metadata --locked --no-deps` / complete offline metadata | Workspace and `interprocess` 2.4.8 verified using complete metadata. |
| `cargo test --workspace --all-targets --locked` | Compiled successfully; **0 Rust tests** executed in three test binaries. |
| `uv lock --check`; `uv run --isolated --locked --extra test python -m pytest -q` | Lock verified; **1 Python test passed**. Non-isolated run failed due to Windows access permissions inside `.venv`; no repair attempted. |
| `node .yarn/releases/yarn-4.18.1.cjs test` | **1 Node duplex test passed**. Separate half-close functional test used an existing local `.node` artifact; its provenance from the current HEAD was not established, with source inspection performed separately. |
| **Rust dependencies** | OSV batch query covering **106** unique crates.io name/version pairs in `Cargo.lock`: no findings. Local `cargo-audit` installation in a scratch directory timed out; OSV does not cover every RustSec advisory category. [14] |
| **Python dependencies** | `pip-audit` against the pinned export of runtime dependencies: no known findings; no conclusion for build/test extras. |
| **Node dependencies** | `yarn npm audit --all --recursive` reported moderate advisories `GHSA-3g3m-2fjr-6pw4` (`glob` 13.0.7) and `GHSA-g3hv-75wg-m249` (`minimatch` 10.2.6); `yarn why` confirmed `@vercel/nft → glob → minimatch`. Exploitability in Taunicorn was not assessed. [15][16] |

### 5.4 🔑 Lockfile fingerprints (historical)

| File | SHA-256 |
|:--|:--|
| `Cargo.lock` | `f2274fd0aca2591c53e8bea226fa573652d5d3e0f69b212d78f25115bdb28b60` |
| `uv.lock` | `8f9a93b8eb105a8986084829890b4a7e35d2493182d2adb1d65f51021f883013` |
| `yarn.lock` | `f035d865960f190375eaccdc1cfce69cfa4f9aae450fd2dedd9158278821db3e` |

### 5.5 🧷 Original verdict & scope exclusions

**Original verdict:** Enforce OS peer authorization and resource bounds **before** the handshake. Next, correct Secure-v2 cancellation semantics and extend the existing duplex API with an explicitly selected message profile.

**Not tested in that baseline:** native runtime behavior on Linux/macOS/BSD; Windows with different user accounts and SMB; artifact provenance; systematic load/cryptographic testing; or operator-specific legal applicability and distribution-model review. See the newer reassessments at the beginning for subsequent progress.

---

## 📚 Sources & Technical References

The original report's reference numbering is preserved (including its omission of `[10]`). Links are provided for traceability; inclusion does not imply a new independent legal verification of the source report.

| Ref. | Reference |
|:--|:--|
| **[1]** | [GDPR — Regulation (EU) 2016/679](https://eur-lex.europa.eu/eli/reg/2016/679) |
| **[2]** | [NIS2 — Directive (EU) 2022/2555](https://eur-lex.europa.eu/eli/dir/2022/2555/en) |
| **[3]** | [German BSIG § 30](https://www.gesetze-im-internet.de/bsig_2025/__30.html) |
| **[4]** | [German BSIG § 32](https://www.gesetze-im-internet.de/bsig_2025/__32.html) |
| **[5]** | [Cyber Resilience Act — Regulation (EU) 2024/2847 (PDF)](https://eur-lex.europa.eu/legal-content/EN/TXT/PDF/?uri=CELEX%3A32024R2847) |
| **[6]** | [Data Act — Regulation (EU) 2023/2854](https://eur-lex.europa.eu/legal-content/EN/TXT/?qid=1748727099517&uri=CELEX%3A32023R2854) |
| **[7]** | [eIDAS — Regulation (EU) No 910/2014](https://eur-lex.europa.eu/eli/reg/2014/910) |
| **[8]** | [Regulation (EU) 2024/1183 — European Digital Identity Framework](https://eur-lex.europa.eu/eli/reg/2024/1183/oj) |
| **[9]** | [Linux `unix(7)` manual](https://man7.org/linux/man-pages/man7/unix.7.html) |
| **[11]** | [Microsoft — `CreateNamedPipeW`](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew) |
| **[12]** | [Microsoft — Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights) |
| **[13]** | [Microsoft — `ImpersonateNamedPipeClient`](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient) |
| **[14]** | [OSV — Query Batch API](https://google.github.io/osv.dev/post-v1-querybatch) |
| **[15]** | [GitHub Advisory GHSA-3g3m-2fjr-6pw4](https://github.com/advisories/GHSA-3g3m-2fjr-6pw4) |
| **[16]** | [GitHub Advisory GHSA-g3hv-75wg-m249](https://github.com/advisories/GHSA-g3hv-75wg-m249) |
| **[17]** | [Consolidated eIDAS text](https://eur-lex.europa.eu/legal-content/en/TXT/?uri=CELEX%3A02014R0910-20241018) |
| **[18]** | [European Commission — CRA Reporting](https://digital-strategy.ec.europa.eu/en/policies/cra-reporting) |
| **[19]** | [freedesktop.org — Base Directory Specification](https://specifications.freedesktop.org/basedir/latest) |
| **[20]** | [NIS2 Directive (PDF)](https://eur-lex.europa.eu/legal-content/EN/TXT/PDF/?qid=1703015085847&uri=CELEX%3A32022L2555) |

---

<sub>**Document type:** English translation and Markdown presentation of the supplied German audit · **Source assessment date:** 22 November 2026 · **Editorial changes:** layout, headings, icons, and readable table formatting; technical conclusions remain attributed to the original audit.</sub>
