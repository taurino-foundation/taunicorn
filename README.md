<div align="center">
  <img
    src="https://raw.githubusercontent.com/taurino-foundation/taunicorn/main/docs/images/logo.png"
    alt="Taunicorn"
    width="50%"
  >
</div>

> **🚧 Currently Under Development 🚧** — We are actively building the core of Taunicorn.

---

# Taunicorn

**Asynchronous local IPC for Rust and Python.**

Taunicorn is a small cross-platform IPC library built around **named local endpoints** and **ordered full-duplex byte streams**.

The Rust API is concrete and struct-based. The Python API is implemented with PyO3 and `pyo3-async-runtimes`, exposing Python `asyncio` awaitables while the actual transport I/O runs on Tokio.

Taunicorn deliberately does **not** impose RPC, serialization, routing, framing, retries, acknowledgements, sessions, or broker semantics.

> Taunicorn transports bytes. Your application decides what those bytes mean.

## Features

- Async Rust transport built on Tokio
- Concrete, struct-based Rust API
- Async Python API through PyO3 and `pyo3-async-runtimes`
- Python `asyncio` integration with Tokio-backed Rust futures
- Ordered full-duplex byte streams
- Concurrent receive and send on one connection
- Same-direction write serialization
- Multiple independent clients per server
- Named local endpoints
- Connect, accept, read, and complete-write timeouts
- Explicit EOF and half-close semantics
- Explicit read/write halves through `into_split()`
- Opaque byte payloads
- Cross-platform local IPC
- PEP 561 typing via `_taunicorn.pyi` and `py.typed`
- No broker, RPC layer, serializer, message envelope, reconnect, or replay layer

## Status

Taunicorn is currently **alpha software**.

The transport contract is intentionally small:

- one `SocketServer` represents one named local endpoint;
- one server can accept multiple independent `SocketConnection` values;
- every connection is an ordered full-duplex byte stream;
- `receive()` and `send()` may make progress concurrently;
- concurrent sends are serialized so their buffers do not interleave;
- Taunicorn does not interpret payloads;
- Taunicorn does not preserve application write boundaries;
- a successful local send is not an application-level acknowledgement;
- peer EOF closes only the peer-to-local direction;
- local read and write directions can be shut down independently;
- retries, framing, authentication, sessions, and application protocol behavior stay above the transport.

## Installation

```bash
python -m pip install taunicorn
```

or with `uv`:

```bash
uv add taunicorn
```

Python 3.10 or newer is required.

## Python quick start

The primary Python API mirrors the concrete Rust transport model:

```python
import asyncio

from taunicorn import SocketConnection, SocketServer


async def read_exact(connection: SocketConnection, size: int) -> bytes:
    data = bytearray()

    while len(data) < size:
        chunk = await connection.receive(size - len(data))
        if not chunk:
            raise EOFError("peer closed its sending direction")
        data.extend(chunk)

    return bytes(data)


async def serve_once(server: SocketServer) -> None:
    connection = await server.accept()

    try:
        request = await read_exact(connection, 4)
        assert request == b"PING"

        await connection.send(b"PONG")
        await connection.shutdown_write()
    finally:
        await connection.close()


async def main() -> None:
    server = await SocketServer.start("taunicorn-example")
    server_task = asyncio.create_task(serve_once(server))

    client = await SocketConnection.connect("taunicorn-example")

    try:
        await client.send(b"PING")

        response = await read_exact(client, 4)
        print(response)  # b"PONG"

        # The server half-closed its sending direction.
        assert await client.receive(1) == b""
        assert client.at_eof()

        await client.shutdown_write()
    finally:
        await client.close()
        await server_task
        await server.stop()


asyncio.run(main())
```

`SocketListener`, `SocketStream`, and `SocketClient` remain available as compatibility aliases for the new concrete classes:

```python
from taunicorn import SocketListener, SocketStream

assert SocketListener is SocketServer
assert SocketStream is SocketConnection
```

New code should prefer `SocketServer` and `SocketConnection`.

## Byte-stream semantics

Taunicorn exposes a **stream**, not a message transport.

These sends:

```python
await connection.send(b"hello")
await connection.send(b"world")
```

do not guarantee two corresponding receives.

The peer may observe:

```text
b"helloworld"
```

in one receive, or any sequence of smaller chunks that preserves byte order.

If your application needs message boundaries, add framing above Taunicorn, for example:

- fixed-size records;
- newline-delimited records;
- length-prefixed frames;
- JSON, MessagePack, Protobuf, or another application protocol.

Taunicorn deliberately does not choose one.

## Full-duplex I/O

A `SocketConnection` supports one receive path and one send path making progress concurrently.

Python:

```python
await asyncio.gather(
    receive_from(connection),
    send_to(connection),
)
```

Rust:

```rust
let (received, sent) = tokio::join!(
    connection.receive(&mut buffer),
    connection.send(b"ping"),
);
```

Same-direction sends are serialized by the Rust connection so complete-buffer sends do not interleave with each other.

The Python wrapper does not create a second socket state machine. It delegates transport state, ordering, EOF, shutdown, and close behavior to the Rust structs.

## EOF and half-close

For a non-zero receive size, Python returns `b""` when the peer has cleanly closed its sending direction:

```python
while chunk := await connection.receive(64 * 1024):
    process(chunk)

assert connection.at_eof()
```

EOF does **not** necessarily mean the whole connection is closed.

The local sending direction may still be usable:

```python
chunk = await connection.receive(4096)

if chunk == b"":
    # Peer will send no more data, but we may still send our final response.
    await connection.send(b"final response")
    await connection.shutdown_write()
```

Directional shutdown:

```python
await connection.shutdown_read()
await connection.shutdown_write()
```

Full close:

```python
await connection.close()
```

Server shutdown:

```python
await server.stop()
```

`server.close()` is a synchronous compatibility operation for stopping the listener state.

## Timeouts

```python
connection = await SocketConnection.connect_timeout("endpoint", 2.0)
connection = await server.accept_timeout(2.0)

chunk = await connection.read_timeout(64 * 1024, 5.0)
await connection.write_all_timeout(payload, 5.0)
```

Timeouts are expressed in seconds.

### Write-timeout and cancellation safety

A complete-buffer write can fail or be cancelled after the operating system has already accepted a prefix of the supplied bytes.

Blindly retrying the complete payload could therefore duplicate bytes at the peer.

For this reason:

- a failed complete-buffer write is treated conservatively;
- `write_all_timeout()` closes the connection when partial progress may have happened;
- Python-side cancellation of an in-progress send causes the binding to close or shut down the affected write path conservatively.

Retry, acknowledgement, replay, deduplication, and exactly-once behavior belong in a higher-level application protocol.

## Python API

### `SocketEndpoint`

A validated named local endpoint.

```python
from taunicorn import SocketEndpoint

endpoint = SocketEndpoint("taunicorn-example")

print(endpoint.name)
print(str(endpoint))
```

`SocketServer.start()` and `SocketConnection.connect()` accept either a `str` or `SocketEndpoint`.

### `SocketServer`

| API | Purpose |
| --- | --- |
| `await SocketServer.start(endpoint)` | Start a server with default platform permissions |
| `await SocketServer.bind(name, mode=...)` | Bind with a platform-specific mode/security descriptor |
| `await server.accept()` | Accept the next connection |
| `await server.accept_timeout(seconds)` | Accept with timeout |
| `await server.stop()` | Stop accepting and release the listening endpoint |
| `server.close()` | Synchronous compatibility stop |
| `server.pause()` / `server.resume()` | Pause or resume accepting |
| `server.is_accepting()` | Check whether new accepts are enabled |
| `server.is_started()` | Check whether the server has not been stopped |
| `server.is_stopped()` | Check whether the server has been stopped |
| `server.is_closed()` | Compatibility view of stopped state |
| `server.is_paused()` | Check pause state |
| `server.name` | Endpoint name |
| `server.endpoint` | `SocketEndpoint` value |
| `server.info()` | `SocketServerInfo` snapshot |

`mode` is platform-specific:

- Unix: integer file mode;
- Windows: SDDL security descriptor string.

Portable applications should normally use the default.

### `SocketConnection`

| API | Purpose |
| --- | --- |
| `await SocketConnection.connect(endpoint)` | Connect to a server |
| `await SocketConnection.connect_timeout(endpoint, seconds)` | Connect with timeout |
| `await connection.receive(max_bytes)` | Receive an arbitrary byte-stream chunk |
| `await connection.read(max_bytes)` | Alias for `receive()` |
| `await connection.read_timeout(max_bytes, seconds)` | Receive with timeout |
| `await connection.send(data)` | Send the complete buffer |
| `await connection.write(data)` | Perform one possibly-partial write |
| `await connection.write_all(data)` | Alias for complete-buffer send |
| `await connection.write_all_timeout(data, seconds)` | Complete-buffer send with timeout |
| `await connection.flush()` | Flush locally buffered output |
| `await connection.shutdown_read()` | Stop local receives |
| `await connection.shutdown_write()` | Finish the local sending direction |
| `await connection.close()` | Close both directions |
| `connection.into_split()` | Consume wrapper state and return explicit read/write halves |
| `connection.at_eof()` | Check whether peer EOF has been observed |
| `connection.peer_sent_eof()` | Explicit peer EOF diagnostic |
| `connection.is_closed()` | Check full-close state |
| `connection.is_read_shutdown()` | Check local read shutdown |
| `connection.is_write_shutdown()` | Check local write shutdown |
| `connection.is_started()` | Check non-closed state |
| `connection.is_active()` | Check active state |
| `connection.is_available()` | Check active and unpaused state |
| `connection.is_paused()` | Check user-level pause state |
| `connection.pause()` / `connection.resume()` | Pause or resume user-level I/O |
| `connection.id` | Process-local diagnostic identifier |
| `connection.name` | Logical endpoint name |
| `connection.endpoint` | `SocketEndpoint` |
| `connection.local_endpoint` | Optional local endpoint snapshot |
| `connection.peer_endpoint` | Optional peer endpoint snapshot |
| `connection.info()` | `SocketConnectionInfo` snapshot |

`id` is process-local diagnostic information. It is not a persistent session identifier.

### Split connection

`into_split()` exposes independent concrete directional wrappers:

```python
reader, writer = connection.into_split()

await writer.send(b"hello")

data = await reader.receive(4096)

await writer.shutdown_write()
await reader.shutdown_read()
```

`SocketReadHalf` exposes:

- `receive()`
- `read()`
- `shutdown_read()`
- `id`
- `info()`

`SocketWriteHalf` exposes:

- `send()`
- `write()`
- `flush()`
- `shutdown_write()`
- `id`
- `info()`

Splitting itself is synchronous. It fails if another async operation still owns a temporary reference to the unsplit connection.

### `LocalSocketTransport`

`LocalSocketTransport` is a concrete zero-sized namespace, not a trait:

```python
from taunicorn import LocalSocketTransport

server = await LocalSocketTransport.start("my-endpoint")
connection = await LocalSocketTransport.connect("my-endpoint")
```

Most application code can use `SocketServer` and `SocketConnection` directly.

### Compatibility aliases

The package keeps these aliases for existing code:

```python
SocketListener = SocketServer
SocketStream = SocketConnection
SocketClient = SocketConnection
```

They do not represent a separate implementation.

## Python exceptions

| Condition | Python exception |
| --- | --- |
| Invalid endpoint or timeout | `ValueError` |
| Invalid endpoint argument type | `TypeError` |
| Timeout | `TimeoutError` |
| Closed/stopped/shut-down transport | `ConnectionError` |
| Paused transport | `BlockingIOError` |
| OS I/O failure | `OSError` |
| Wrapper state or unclassified transport failure | `RuntimeError` |

Catch the narrowest exception relevant to the operation.

## Rust API

The Rust transport is concrete and struct-based.

Primary types:

```rust
use taunicorn_transport::{
    LocalSocketTransport,
    ReceiveResult,
    SocketConnection,
    SocketConnectionInfo,
    SocketEndpoint,
    SocketReadHalf,
    SocketServer,
    SocketServerInfo,
    SocketWriteHalf,
};
```

### Server

```rust
use anyhow::Result;
use taunicorn_transport::SocketServer;

async fn run() -> Result<()> {
    let server = SocketServer::start("taunicorn-example").await?;
    let connection = server.accept().await?;

    // use connection ...

    connection.close().await?;
    server.stop().await?;

    Ok(())
}
```

### Connection

```rust
use anyhow::Result;
use taunicorn_transport::{ReceiveResult, SocketConnection};

async fn exchange() -> Result<()> {
    let connection = SocketConnection::connect("taunicorn-example").await?;

    connection.send(b"PING").await?;

    let mut buffer = [0_u8; 4096];

    match connection.receive(&mut buffer).await? {
        ReceiveResult::Data(n) => {
            println!("received {} bytes", n);
        }
        ReceiveResult::EndOfStream => {
            println!("peer closed its sending direction");
        }
    }

    connection.shutdown_write().await?;
    connection.close().await?;

    Ok(())
}
```

### Split connection

```rust
let connection = SocketConnection::connect("taunicorn-example").await?;

let (reader, writer) = connection.into_split();

let read_task = tokio::spawn(async move {
    let mut buffer = [0_u8; 4096];
    reader.receive(&mut buffer).await
});

let write_task = tokio::spawn(async move {
    writer.send(b"PING").await?;
    writer.shutdown_write().await
});
```

The transport layer deliberately exposes no custom trait hierarchy for normal use.

## Architecture

```text
Python application
        │
        │ await
        ▼
python/taunicorn
        │
        ▼
crates/taunicorn-python
PyO3 + pyo3-async-runtimes
        │
        │ Rust Future
        ▼
Tokio runtime
        │
        ▼
crates/taunicorn-transport
SocketServer / SocketConnection
        │
        ▼
interprocess local sockets
        │
        ▼
OS local IPC
```

Python owns and drives its `asyncio` event loop.

Rust asynchronous I/O runs on the Tokio runtime managed by `pyo3-async-runtimes`.

The bridge converts Rust futures into Python awaitables; it does not run Rust futures directly on the Python event loop.

## Platform targets

Primary release wheel targets:

| Platform | Architecture |
| --- | --- |
| Linux | x86_64 |
| Linux | aarch64 |
| Windows | x86_64 |
| macOS | x86_64 |
| macOS | arm64 / aarch64 |

The release pipeline also publishes a source distribution.

Additional Unix-like systems may build from source, but should only be considered release-tier platforms once covered by the project's own CI.

## Repository layout

```text
taunicorn/
├── .github/
│   ├── dependabot.yml
│   └── workflows/
│       ├── rust-ci.yml
│       ├── python-ci.yml
│       ├── security.yml
│       ├── publish-python.yml
│       └── publish-rust.yml
├── crates/
│   ├── taunicorn-python/
│   │   ├── Cargo.toml
│   │   └── src/
│   └── taunicorn-transport/
│       ├── Cargo.toml
│       └── src/
├── docs/
├── python/
│   ├── taunicorn/
│   │   ├── __init__.py
│   │   ├── _taunicorn.pyi
│   │   └── py.typed
│   └── tests/
├── Cargo.toml
├── Cargo.lock
├── pyproject.toml
├── rustfmt.toml
├── README.md
├── LICENSE
├── LICENSE-APACHE-2.0
└── LICENSE-MIT
```

## Development

Requirements:

- Rust stable
- Python 3.10+
- `uv`
- Maturin through the project build configuration

### Rust transport

```bash
cargo fmt --all -- --check
cargo check -p taunicorn-transport --all-targets --locked
cargo clippy -p taunicorn-transport --all-targets --locked -- -D warnings
cargo test -p taunicorn-transport --all-targets --locked
```

### PyO3 crate

```bash
cargo check -p taunicorn-python --all-targets --locked
cargo clippy -p taunicorn-python --all-targets --locked -- -D warnings
```

### Python

Create the development environment and install the project according to the root `pyproject.toml`, then run:

```bash
uv run pytest python/tests
```

Build Python distributions:

```bash
uv build
```

## Packaging

Taunicorn uses Maturin as its PEP 517 backend.

The repository root `pyproject.toml` should point Maturin at the dedicated PyO3 crate:

```toml
[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"

[tool.maturin]
bindings = "pyo3"
manifest-path = "crates/taunicorn-python/Cargo.toml"
python-source = "python"
module-name = "taunicorn._taunicorn"
```

The Python package itself lives under:

```text
python/taunicorn/
```

Application code should import from `taunicorn`, not directly from the private native extension `taunicorn._taunicorn`.

## Typing

Taunicorn ships PEP 561 type information alongside the Python package:

```text
python/taunicorn/_taunicorn.pyi
python/taunicorn/py.typed
```

The stub describes the actual Python-facing API, including:

- awaitable transport operations;
- synchronous state/diagnostic methods;
- `SocketEndpoint`;
- `SocketConnectionInfo`;
- `SocketServerInfo`;
- split read/write halves;
- compatibility aliases.

## Continuous integration

The repository separates Rust, Python, security, and publishing responsibilities.

### Rust CI

`.github/workflows/rust-ci.yml`

Checks:

- `cargo fmt`
- `cargo check`
- `cargo clippy -D warnings`
- native transport tests on Linux
- native transport tests on Windows
- native transport tests on macOS Intel
- native transport tests on macOS ARM64

### Python CI

`.github/workflows/python-ci.yml`

Checks:

- Python 3.10–3.14 on Linux
- Python 3.14 on Windows x86_64
- Python 3.14 on macOS x86_64
- Python 3.14 on macOS ARM64
- native wheel build
- wheel installation
- `python/tests`
- real `asyncio -> PyO3 -> Tokio -> local socket` integration
- source-distribution build and install test

### Security CI

`.github/workflows/security.yml`

Checks:

- Rust dependencies with `cargo-audit`
- Python runtime dependencies with `pip-audit`
- dependency-file changes
- scheduled security audits

## Dependabot

`.github/dependabot.yml` should cover all three dependency surfaces:

```yaml
version: 2

updates:
  - package-ecosystem: "github-actions"
    directory: "/"
    target-branch: "master"
    schedule:
      interval: "monthly"
    groups:
      github-actions:
        patterns:
          - "*"

  - package-ecosystem: "cargo"
    directory: "/"
    target-branch: "master"
    schedule:
      interval: "monthly"
    groups:
      rust-dependencies:
        patterns:
          - "*"

  - package-ecosystem: "uv"
    directory: "/"
    target-branch: "master"
    schedule:
      interval: "monthly"
    groups:
      python-dependencies:
        patterns:
          - "*"
```

If `master` is the repository default branch, `target-branch` may be omitted.

## Releases

Python and Rust publishing are intentionally separated.

### PyPI

`.github/workflows/publish-python.yml`

A published GitHub Release triggers:

```text
Git tag
   │
   ▼
validate taunicorn-python version
   │
   ▼
sdist + platform wheels
   │
   ▼
wheel smoke tests
   │
   ▼
PEP 740 attestations
   │
   ▼
PyPI Trusted Publishing
```

The PyPI job uses GitHub OIDC and should not require a long-lived PyPI API token.

Create a GitHub environment named:

```text
pypi
```

and configure the corresponding PyPI Trusted Publisher for the exact repository and workflow.

### crates.io

`.github/workflows/publish-rust.yml`

Rust publishing is separate and guarded.

The workflow:

1. runs `cargo publish --dry-run` for `taunicorn-transport`;
2. only publishes when explicitly requested;
3. uses a protected `crates-io` GitHub environment;
4. reads `CARGO_REGISTRY_TOKEN` only in the actual publish job.

## Design principles

**Transport, not protocol.**

Taunicorn transports ordered bytes and leaves application semantics above the transport.

**Concrete APIs over transport trait hierarchies.**

The core public implementation is expressed directly through `SocketServer`, `SocketConnection`, `SocketReadHalf`, and `SocketWriteHalf`.

**One transport state machine.**

The Python binding delegates connection state, EOF, ordering, shutdown, and close behavior to the Rust transport rather than duplicating it.

**Explicit failure behavior.**

Timeout, EOF, shutdown, cancellation, and OS failures remain visible rather than being hidden behind automatic retry.

**Local IPC first.**

Remote networking, service discovery, TLS, and distributed-system semantics are outside the core scope.

**Higher-level guarantees stay higher-level.**

Framing, RPC, acknowledgements, retries, persistence, reconnect, replay, deduplication, and exactly-once behavior belong in optional layers or application protocols.

## Security

Local IPC should not automatically be treated as trusted IPC.

Applications should:

- choose endpoint names deliberately;
- use appropriate OS-level access controls;
- validate untrusted application payloads;
- impose maximum frame or payload sizes above the raw stream;
- avoid treating a successful send as proof that the peer processed the data;
- avoid blindly retrying complete buffers after ambiguous partial-write failures.

Taunicorn does not authenticate application payloads or provide application-level encryption.

## Contributing

Before opening a pull request:

```bash
cargo fmt --all -- --check

cargo check -p taunicorn-transport --all-targets --locked
cargo clippy -p taunicorn-transport --all-targets --locked -- -D warnings
cargo test -p taunicorn-transport --all-targets --locked

cargo check -p taunicorn-python --all-targets --locked
cargo clippy -p taunicorn-python --all-targets --locked -- -D warnings

uv run pytest python/tests
```

Changes that introduce framing, RPC, serialization, routing, persistence, retry/replay, or broker behavior should generally live above the raw transport instead of becoming implicit `SocketConnection` behavior.

## License

See `LICENSE`, `LICENSE-APACHE-2.0`, and `LICENSE-MIT`.
