# Security Policy

## Supported Versions

Taunicorn is currently **alpha software**.

Security fixes are provided for:

| Version        | Supported |
| -------------- | --------- |
| Latest release | ✅         |
| `main` branch  | ✅         |
| Older releases | ❌         |

Users are encouraged to reproduce security issues against the latest release or the current `main` branch before reporting them.

## Reporting a Vulnerability

Please **do not open a public GitHub issue** for a suspected security vulnerability.

Report security vulnerabilities privately using GitHub's security reporting facilities for this repository.

When reporting a vulnerability, please include as much of the following information as possible:

* affected Taunicorn version or commit;
* affected operating system and architecture;
* whether the issue affects the Rust API, Python API, or both;
* a description of the vulnerability and its potential impact;
* minimal reproduction steps or a proof of concept;
* relevant logs, stack traces, or error messages;
* any known workarounds or mitigations.

Please avoid including sensitive information that is not necessary to reproduce the issue.

## What Counts as a Security Issue

Examples of issues that may qualify as security vulnerabilities include:

* unintended access to a local IPC endpoint;
* incorrect or bypassable endpoint permission handling;
* memory-safety vulnerabilities;
* exploitable crashes, panics, or resource-exhaustion conditions triggered by an untrusted peer;
* unsafe handling of endpoint names or platform-specific IPC resources;
* vulnerabilities in the Python/Rust boundary that could violate memory or process safety;
* behavior that materially contradicts documented security guarantees.

## Security Boundaries

Taunicorn is a **local IPC byte-stream transport**, not an authentication, authorization, encryption, RPC, or application-protocol layer.

Applications using Taunicorn are responsible for:

* choosing endpoint names appropriately;
* configuring suitable operating-system access controls;
* validating untrusted application payloads;
* enforcing application-level payload or frame size limits;
* implementing authentication or authorization when required;
* implementing application-level encryption when required;
* handling retries, acknowledgements, replay protection, and deduplication when required.

A successful transport-level send does not prove that the peer authenticated, validated, or processed the transmitted data.

Issues caused solely by an application failing to provide these higher-level controls are generally not vulnerabilities in Taunicorn itself.

## Coordinated Disclosure

Please allow maintainers reasonable time to investigate and address a confirmed vulnerability before publishing technical details.

After triage, maintainers may coordinate with the reporter on:

* severity and impact;
* affected versions;
* mitigations;
* release timing;
* a GitHub Security Advisory or CVE, when appropriate.

We appreciate responsible disclosure and reports that help improve Taunicorn's security.
