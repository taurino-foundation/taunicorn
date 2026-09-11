<div align="center">
  <img
    src="https://raw.githubusercontent.com/taurino-foundation/taunicorn/main/docs/images/logo.png"
    alt="Taunicorn"
    width="50%"
  >
</div>

# Taunicorn

Asynchronous local inter-process communication for Rust and Python.

Taunicorn connects processes through named local endpoints and ordered,
full-duplex byte streams. Rust applications use a concrete Tokio-based API.

Python applications use `asyncio` awaitables backed by the same native transport
through PyO3 and `pyo3-async-runtimes`.

The core transport moves bytes without defining their meaning. An optional
`SecureConnection` layer adds authenticated, encrypted messages. Python now uses
that existing Rust implementation through a PyO3 binding instead of maintaining
its own cryptography implementation. Separate Rust-backed queues provide
in-process coordination for Python objects.

The migration replaces the Python security implementation only. The Rust core,
its cryptographic algorithms, and the versioned wire protocol remain unchanged.

For the integration's build and test status, see
[Native security integration status](#native-security-integration-status).

> [!NOTE]
> Taunicorn is alpha software. Review the transport contract, failure behavior,
> and security requirements before depending on it in a production system.

## Contents

- [Capabilities and system behavior](#capabilities-and-system-behavior)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Installation and setup](#installation-and-setup)
- [Configuration](#configuration)
- [Usage](#usage)
- [Technical reference](#technical-reference)
- [Limitations and known risks](#limitations-and-known-risks)
- [Troubleshooting](#troubleshooting)
- [Security and operations](#security-and-operations)
- [Development and releases](#development-and-releases)
- [License](#license)

## Capabilities and system behavior

Choose the API according to the boundary it provides:

| Component | Data model | Responsibility |
| --- | --- | --- |
| `Server` / `Connection` | Ordered local byte stream | Connection lifecycle, concurrent send/receive, timeouts, and directional shutdown |
| `SecureConnection` | Authenticated, encrypted messages | Peer authentication, framing, encryption, integrity, and per-session sequence checks |
| `BoundedQueue` / `UnboundedQueue` | In-process FIFO of Python objects | Producer/consumer coordination through Rust-backed channels |

The raw transport contract is deliberately small:

- One server owns one named endpoint and can accept multiple independent connections.
- Sending and receiving may progress concurrently. Same-direction operations are serialized; concurrent complete-buffer sends do not interleave their bytes.
- A receive returns a stream chunk, not necessarily one application write. Applications define framing unless they use the secure message layer.
- Peer EOF, local read shutdown, local write shutdown, and full connection closure are distinct states.
- A completed send or flush is not an acknowledgement that the peer processed, accepted, or committed an operation.

RPC, routing, service discovery, reconnect, persistence, application
acknowledgements, and exactly-once processing are outside the raw transport.

## Architecture

### Runtime and protocol boundaries

Python and Rust share the native byte transport and the same Rust secure-session
implementation. The public Python security module is a small coroutine facade:
it delegates handshake, framing, identity operations, encryption, decryption,
and sequence checks to the native extension. It contains no independent
cryptographic protocol implementation.

The secure path is:

```text
Python application                         Rust application
        |                                          |
Python SecureConnection facade                     |
(optional Python MessagePack)                      |
        |                                          |
PyO3 / pyo3-async-runtimes                           |
        |                                          |
        +--------------------+---------------------+
                             |
          Existing Rust SecureConnection
            (optional secure message layer)
                             |
                  Tokio Server / Connection
                             |
                 interprocess local sockets
                             |
                        OS local IPC
```

Raw transport users bypass the optional secure message layer. Python owns its
`asyncio` event loop; the async bridge exposes Rust futures as Python awaitables.

Native transport I/O and secure handshake/message operations execute on Tokio.

Connection state, ordering, EOF, and directional shutdown remain owned by the
Rust transport. The Python binding adds its own secure-session ownership and
terminal-on-failure policy without modifying the Rust core.

The public facade retains `async def` constructors and message methods, including
compatibility with `asyncio.create_task(secure.receive())`. Its underlying native
methods return awaitables rather than Python coroutine objects. Identity helpers
are synchronous native calls that detach from Python during their Rust work;
they are not asynchronous Tokio tasks.

Optional MessagePack serialization and deserialization remain in Python to
preserve the existing Python data mapping. That work is synchronous and is not
automatically offloaded to Tokio. Python/native conversions and buffer copying
also remain part of the call path.

The Python queues use Tokio MPSC channels and serialized receiver access. They
hold Python objects within a process; they are not an IPC message broker or a
serialization mechanism.

### Python security migration

The previous Python security module performed its own handshake, framing, and
cryptography using the PyPI `cryptography` package. The replacement calls the
existing Rust implementation through `taunicorn._taunicorn.SecureConnection`.

This removes that security layer's dependency on a separate Python cryptography
wheel and puts its native code in Taunicorn's Maturin-built extension.

| Component | Migration scope |
| --- | --- |
| Rust core, `crates/taunicorn/` | Unchanged transport, security protocol, and core manifest |
| Python binding, `crates/taunicorn-python/` | Native secure class, identity helpers, module registration, ownership transfer, and cancellation handling |
| Public Python package, `python/taunicorn/` | Replace Python cryptography with a coroutine facade; retain optional Python MessagePack helpers and update typing/exports |

Public constructors, keyword arguments, byte-oriented methods, and identity
helpers remain available through `taunicorn.security`. Internal Python cipher
objects and `_DirectionState` no longer exist. Raw I/O after an upgrade and
recovery after native secure-operation failure are deliberately more restrictive;
see [Secure failure recovery](#secure-failure-recovery).

The Rust core is reused, not rewritten or replaced. Removing `cryptography` does
not by itself prove that every ABI or wheel problem in the application is fixed;
the Taunicorn extension still needs a compatible build for its target.

### Connection lifecycle

`Server.start()` creates a listener. Each successful `accept()` returns an
independent `Connection`; `Connection.connect()` creates the client side.

Stopping the server interrupts pending accepts and releases the listener once
in-flight operations release their references. It does not close already
accepted connections. Applications must manage those connections separately.

A secure session starts with an ordinary, unsplit connection. The client and
server perform the secure handshake before exchanging application messages.

The Python binding transfers exclusive ownership of the native transport to the
existing Rust secure constructor. The upgrade is rejected while a raw async
operation still holds the connection; an ownership-check rejection leaves that
raw connection in place.

After a successful upgrade, `secure.connection is connection` remains true.

That original Python object supports diagnostics and `close()`, but raw reads,
writes, flushes, directional shutdown, and `into_split()` are disabled. All
application traffic must use the secure API. The guard also applies to aliases
of the same Python object.

During the handshake, the raw wrapper is unavailable for I/O and normal
diagnostics. Calling `connection.close()` requests cancellation of the upgrade.

A handshake that fails or is cancelled after ownership transfer leaves the
wrapper unusable for further I/O; establish a new connection instead of
falling back to plaintext.

## Requirements

### Runtime and build requirements

| Use case | Requirements |
| --- | --- |
| Python transport | Python 3.10 or newer, a running `asyncio` loop for asynchronous operations, and the native extension |
| Python secure byte messages | Python transport with the native security binding; no `cryptography` or `msgpack` required by this layer |
| Python secure MessagePack helpers | Native secure byte messages plus the optional `msgpack` package |
| Rust applications | Rust with Cargo and a Tokio runtime |
| Source development | Rust stable, Python 3.10 or newer, `uv`, and the Maturin build configuration |

The Python security module imports its secure class and identity helpers from
the Taunicorn native extension. It does not import `cryptography`. `msgpack` is
loaded only when `send_msgpack()` or `receive_msgpack()` is called; importing the
module and exchanging plaintext bytes through the encrypted session do not
require it. No fallback to the former Python cryptography implementation is
provided.

The binding retains the PyO3 `abi3-py310` build feature. This targets the CPython
stable ABI from Python 3.10 for compatible GIL-enabled interpreters, not a
platform-independent binary. See [Packaging and typing](#packaging-and-typing).

Dependency versions, feature flags, and any minimum supported Rust version must
be taken from the checked-out project manifests. This README does not define a
separate set of dependency pins.

### Platform targets

The documented primary Python wheel targets are:

| Platform | Architectures |
| --- | --- |
| Linux | `x86_64`, `aarch64` |
| Windows | `x86_64` |
| macOS | `x86_64`, `arm64` / `aarch64` |

A source distribution is also part of the documented release process. Wheel
availability depends on the specific release. Other Unix-like platforms are
source-build candidates, not an implied release or CI support guarantee.

## Installation and setup

### Python

Install the Python package into the environment that will run the application:

```bash
python -m pip install taunicorn
```

For a project managed by `uv`, add the package as a project dependency instead:

```bash
uv add taunicorn
```

For the native security implementation, `cryptography` is no longer required.

Install `msgpack` only when using the structured-message convenience methods
and the environment does not already provide it:

```bash
python -m pip install msgpack
```

A `taunicorn[msgpack]` extra may be used once it is declared in the installed
release's packaging metadata. The integration includes an example declaration;
this README does not assume that an already published release contains it.

Use `taunicorn` for transport imports and `taunicorn.security` for the public
secure facade and identity helpers. The `taunicorn._taunicorn` extension is an
implementation detail. The facade and extension must come from the same build:
replacing `security.py` alone is insufficient because older extensions do not
export the new native security class and helpers.

These installation commands select the available package release; they do not
establish that the source integration documented here has already been published.

Build the integrated source checkout when testing this migration.

### Rust

Add the transport crate to an existing Cargo project:

```bash
cargo add taunicorn
```

The runnable Rust example below also uses `anyhow` and Tokio's runtime and macro
support. Add them when they are not already project dependencies:

```bash
cargo add anyhow
cargo add tokio --features macros,rt-multi-thread
```

### Source checkout

From a complete checkout containing the root `pyproject.toml` and Cargo workspace,
create the Python development environment using the project configuration:

```bash
uv sync
```

Source installation requires a working native build toolchain. Build and test
commands are listed under [Development and releases](#development-and-releases).

## Configuration

Taunicorn is configured through API arguments. The transport API does not define
a daemon configuration file or an environment-variable configuration interface.

| Setting | API location | Contract |
| --- | --- | --- |
| Endpoint name | `Endpoint`, `Server.start()`, `Connection.connect()` | Non-empty logical name mapped by the native backend; not a portable filesystem path |
| Endpoint permissions | `Server.bind(..., mode=...)` in Python | Unix permission integer or Windows SDDL string; `None` uses backend/platform defaults |
| Operation timeout | Methods ending in `_timeout` | Explicit seconds in Python; `std::time::Duration` in Rust |
| Queue capacity | `BoundedQueue(capacity)` | Positive supported capacity; determines the number of buffered items, not their byte size |
| Local identity | Secure-session constructor | Raw 32-byte Ed25519 private identity key |
| Expected peer identity | Secure-session constructor | Pinned raw 32-byte Ed25519 public key for the peer |
| Secure frame ceiling | Protocol implementation | Fixed at 64 MiB of frame body; not a constructor option |

Python timeout values must be finite, non-negative, and representable by the
native duration type. The timeout methods require an explicit value; the example
timeouts below are not library defaults.

Permission settings are platform-specific. A default setting, or the same
logical endpoint name on two platforms, does not establish an identical access
control policy. Validate the effective permissions of the deployed backend.

### Identity provisioning

Provision one persistent identity per endpoint. Give each endpoint its own
private key and the other endpoint's public key through the application's
trusted provisioning process.

This code creates the raw key material for one endpoint. Persist the private key
in protected storage; distribute only the public key:

```python
from taunicorn.security import generate_identity_private_key, identity_public_key
private_key = generate_identity_private_key()
public_key = identity_public_key(private_key)
```

The helpers call the existing Rust identity functions and return Python `bytes`.

Private and public identity keys remain raw 32-byte values; the migration does
not change identity storage formats or require regenerating valid keys.

Identity storage, key-file formats, rotation, and trust updates are application
responsibilities. Replacing a private identity requires updating the peer's pin.

Do not regenerate a persistent identity for every production connection.

## Usage

### Python: request and response over a byte stream

Save this example as `stream_example.py` and run it with `python stream_example.py`.

It starts one local server and client, exchanges fixed four-byte records, and
closes both sides. The receive helper reconstructs a record across arbitrary
stream chunks; a single receive is not assumed to return four bytes.

```python
import asyncio
from taunicorn import Connection, Server

async def read_exact(connection: Connection, size: int) -> bytes:
    data = bytearray()
    while len(data) < size:
        chunk = await connection.read_timeout(size - len(data), 5.0)
        if not chunk:
            raise EOFError("peer closed before the record was complete")
        data.extend(chunk)
    return bytes(data)

async def serve_once(server: Server) -> None:
    connection = await server.accept_timeout(5.0)
    try:
        if await read_exact(connection, 4) != b"PING":
            raise ValueError("unexpected request")
        await connection.write_all_timeout(b"PONG", 5.0)
        await connection.shutdown_write()
    finally:
        await connection.close()

async def main() -> None:
    endpoint = "taunicorn-stream-example"
    server = await Server.start(endpoint)
    task = asyncio.create_task(serve_once(server))
    try:
        client = await Connection.connect_timeout(endpoint, 5.0)
        try:
            await client.write_all_timeout(b"PING", 5.0)
            await client.shutdown_write()
            print(await read_exact(client, 4))  # b'PONG'
            assert await client.read_timeout(1, 5.0) == b""
            assert client.at_eof()
        finally:
            await client.close()
        await task
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await server.stop()

asyncio.run(main())
```

The fixed-size record is the example's application protocol, not a Taunicorn
transport feature. The client half-closes its sending direction before reading
the response, demonstrating that the two directions have separate lifecycles.

### Rust: establish a connection and read through EOF

Place this complete program in a Cargo binary's `src/main.rs`. After adding the
dependencies listed above, run it with `cargo run`. It pairs one local client
with one accepted connection and sends a four-byte payload.

```rust
use anyhow::Result;
use taunicorn::{Connection, ReceiveResult, Server};
#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = "taunicorn-rust-example";
    let server = Server::start(endpoint).await?;
    let (receiver, sender) = tokio::try_join!(
        server.accept(),
        Connection::connect(endpoint),
    )?;
    sender.send(b"PING").await?;
    sender.shutdown_write().await?;
    let mut buffer = [0_u8; 4096];
    loop {
        match receiver.receive(&mut buffer).await? {
            ReceiveResult::Data(n) => println!("{:?}", &buffer[..n]),
            ReceiveResult::EndOfStream => break,
        }
    }
    receiver.close().await?;
    sender.close().await?;
    server.stop().await?;
    Ok(())
}
```

This is a small local smoke example, not a supervised service. A deployed
application must also bound connection and I/O lifetimes on failure paths.

### Python: authenticated encrypted messages

Save the following as `secure_example.py` and run it with
`python secure_example.py`. It creates two demonstration identities in memory,
pins each public key at the opposite endpoint, and exchanges one encrypted
request and response.

The surrounding timeout and `finally` blocks belong to the application. They
ensure that the raw connection is closed even when the handshake or a secure
operation fails. Production deployments must load persistent identities instead
of generating the demonstration keys below.

```python
import asyncio
from taunicorn import Connection, Server
from taunicorn.security import (
    SecureConnection,
    generate_identity_private_key,
    identity_public_key,
)

async def serve_once(
    server: Server, private_key: bytes, client_public_key: bytes
) -> None:
    connection = await server.accept()
    try:
        secure = await SecureConnection.server(
            connection,
            identity_private_key=private_key,
            client_identity_public_key=client_public_key,
        )
        if await secure.receive() != b"PING":
            raise ValueError("unexpected request")
        await secure.send(b"PONG")
    finally:
        await connection.close()

async def run_demo() -> None:
    client_private = generate_identity_private_key()
    server_private = generate_identity_private_key()
    endpoint = "taunicorn-secure-example"
    server = await Server.start(endpoint)
    task = asyncio.create_task(
        serve_once(server, server_private, identity_public_key(client_private))
    )
    try:
        connection = await Connection.connect(endpoint)
        try:
            secure = await SecureConnection.client(
                connection,
                identity_private_key=client_private,
                server_identity_public_key=identity_public_key(server_private),
            )
            await secure.send(b"PING")
            print(await secure.receive())  # b'PONG'
        finally:
            await connection.close()
        await task
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await server.stop()

async def main() -> None:
    await asyncio.wait_for(run_demo(), timeout=10.0)

asyncio.run(main())
```

Once established, `secure.send()` accepts Python `bytes` and `secure.receive()`
returns one complete authenticated plaintext message as `bytes`. The identity
parameters also require `bytes` containing exactly 32 bytes.

The example keeps the same Python calls while handshake and message processing
run in the existing Rust implementation. No `cryptography` import is needed.

Do not send or receive application data through the underlying raw connection
after the handshake; the Python binding rejects those operations. The original
connection remains usable for diagnostics and the cleanup shown in the example.

### Rust: secure-session constructors

These helpers accept a connected or accepted transport and return an established
secure session. They make the constructor argument order explicit: the client
takes its private key before the server pin; the server takes the client pin
before its private key.

```rust
use anyhow::Result;
use taunicorn::Connection;
use taunicorn::security::SecureConnection;
async fn upgrade_client(
    connection: Connection,
    client_private: [u8; 32],
    server_public: [u8; 32],
) -> Result<SecureConnection> {
    SecureConnection::client(connection, client_private, server_public).await
}
async fn upgrade_server(
    connection: Connection,
    client_public: [u8; 32],
    server_private: [u8; 32],
) -> Result<SecureConnection> {
    SecureConnection::server(connection, client_public, server_private).await
}
```

The caller owns the returned session and must manage its lifetime. Secure Rust
sessions provide `send()`, `receive()`, `send_msgpack()`, `receive_msgpack()`, and
`close()`. Rust identity helpers return and accept 32-byte arrays;
`generate_identity_private_key()` returns an `anyhow::Result`.

### Structured messages

For an already established secure session, this helper serializes one value
into one encrypted frame:

```python
from taunicorn.security import SecureConnection

async def send_job(secure: SecureConnection) -> None:
    await secure.send_msgpack({
        "type": "job.start",
        "job_id": "42",
        "priority": 5,
    })
```

Receive the value with `await secure.receive_msgpack()`. Python retains
`msgpack.packb(value, use_bin_type=True)` and
`msgpack.unpackb(plaintext, raw=False)`. These helpers serialize and deserialize
in the Python facade; they do not pass Python objects through Rust Serde.

Python binary values therefore keep their existing MessagePack binary encoding.

The existing default map-key restriction is also retained: integer map keys are
rejected on decoding.

In Rust, `send_msgpack(&value)` requires `serde::Serialize`, and
`receive_msgpack::<T>()` requires `serde::de::DeserializeOwned`. Rust uses named
MessagePack fields; both peers still need a compatible application schema.

Sharing a cryptographic implementation does not make every Python/Rust data
type interchangeable.

Both Python helpers import `msgpack` before performing secure I/O. A missing
optional dependency therefore raises `ModuleNotFoundError` without consuming a
received frame. A MessagePack decoding error alone does not automatically close
a session after its encrypted message has been successfully authenticated.

Serialization is a convenience layer, not authorization or input validation.

Check decoded field types, identifiers, sizes, and permitted operations.

### Python: queue close and drain

This example buffers an object, closes the sending side, and drains the queue.

Closing does not discard buffered objects. Once the queue is closed and empty,
receiving raises `QueueClosed`.

```python
import asyncio
from taunicorn import BoundedQueue, QueueClosed

async def main() -> None:
    queue = BoundedQueue(16)
    await queue.send({"kind": "work", "id": 1})
    queue.close()
    try:
        while True:
            print(await queue.recv())
    except QueueClosed:
        pass  # Expected completion after the buffered item is consumed.

asyncio.run(main())
```

## Technical reference

### Byte-stream semantics

`receive(max_bytes)` returns up to the requested number of bytes. Success does
not imply that the buffer is full. Applications needing complete records must
accumulate chunks and reject incomplete records at EOF.

Two sends of `b"hello"` and `b"world"` may be received as `b"helloworld"` or as
smaller ordered chunks. Complete-buffer send serialization prevents concurrent
sends from interleaving, but does not create message boundaries at the receiver.

`send()` sends the complete buffer; `write()` performs one potentially partial
write and returns the number of bytes written. Multiple calls to `write()` are
not one atomic application record. `flush()` is not a remote acknowledgement.

### EOF and directional shutdown

| Operation or result | Meaning |
| --- | --- |
| Python raw `receive(n) == b""`, where `n > 0` | Peer EOF has been observed |
| Python raw `receive(0)` | Empty result without establishing EOF |
| Rust `ReceiveResult::EndOfStream` | Peer EOF has been observed |
| Rust `ReceiveResult::Data(0)` | Empty destination buffer; not EOF |
| `shutdown_read()` | Stop local receives without closing the send direction |
| `shutdown_write()` | Finish local sending without closing the receive direction |
| `connection.close()` | Close both directions and interrupt pending local native I/O |
| `server.stop()` | Stop accepting; accepted connections remain independent |

Peer EOF does not imply that the local send direction is closed. Similarly,
`is_closed() == False` does not imply that both directions remain usable.

Closing a connection and stopping a server are idempotent operations.

A secure receive has a different contract: `b""` is a valid decrypted empty
message, not EOF. EOF while reading a secure frame raises `EOFError` in Python
and returns an error in Rust. `SecureConnection` has no authenticated
end-of-session message in the described protocol.

### Timeouts and cancellation

Python provides the following bounded transport operations. Rust provides the
corresponding methods with `Duration` arguments.

| Python call | Bound |
| --- | --- |
| `Connection.connect_timeout(endpoint, seconds)` | Connection establishment |
| `server.accept_timeout(seconds)` | Waiting for a connection |
| `connection.read_timeout(max_bytes, seconds)` | One raw receive operation |
| `connection.write_all_timeout(data, seconds)` | Waiting for the write gate and sending the complete buffer |

A timeout around one raw receive does not bound an entire application record or
secure handshake. Use an application-level deadline for a multi-step exchange.

The secure constructors and message methods do not expose their own timeout
parameter.

The native complete-write timeout distinguishes two cases. A timeout while
waiting for the write gate occurs before that operation has written bytes. A
timeout during the write loop closes the connection conservatively because
partial progress may have occurred. Native I/O write failures also close the
connection.

> [!WARNING]
> A failed or cancelled write may already have delivered a prefix to the peer.
> Do not blindly retry the complete payload. Recovery requires an application
> protocol with explicit acknowledgement, replay, or deduplication semantics.

The Python binding contract closes an unsplit connection when an in-progress
`send()` or `write()` is cancelled. Cancellation of `WriteHalf.send()` shuts
down the write direction instead.

For Python secure sessions, the new binding also treats observed cancellation
of a native handshake, send, or receive as terminal. Further secure I/O is
rejected, and transport cleanup is requested on Tokio. This policy also covers
cancellation of an already queued operation and observation of a cancelled
Python result at the native/Python completion boundary. Use application-level
deadlines as before, but reconnect after a cancelled secure operation.

Do not generalize those binding guards to direct Rust future cancellation. The
unchanged native `send()` loop has no cancellation guard that closes the
connection merely because its future is dropped. Prefer the dedicated
write-timeout API for raw timed writes, and explicitly close and discard failed
or cancelled secure sessions in direct Rust code.

### Python transport API

Use `Server` and `Connection` for new code. `Endpoint(name)` represents a logical
endpoint; server and connection factories accept either a string or an
`Endpoint` where declared.

#### Server operations

| API | Purpose |
| --- | --- |
| `await Server.start(endpoint)` | Create a listener with default platform settings |
| `await Server.bind(name, mode=...)` | Create a listener with platform-specific permissions |
| `await server.accept()` | Accept one independent connection |
| `await server.accept_timeout(seconds)` | Accept with a timeout |
| `await server.stop()` | Stop the listener and interrupt pending accepts |
| `server.close()` | Synchronous compatibility form of listener stop |
| `server.pause()` / `server.resume()` | Gate acceptance of new connections |
| `server.info()` | Obtain a `ServerInfo` diagnostic snapshot |

Server diagnostics include `name`, `endpoint`, `is_started()`, `is_stopped()`,
`is_closed()`, `is_paused()`, and `is_accepting()`.

#### Connection operations

| API | Purpose |
| --- | --- |
| `await Connection.connect(endpoint)` | Establish a raw connection |
| `await Connection.connect_timeout(endpoint, seconds)` | Establish a connection with a timeout |
| `await connection.receive(max_bytes)` | Receive an arbitrary stream chunk |
| `await connection.send(data)` | Send the complete buffer |
| `await connection.write(data)` | Return the count from a possibly partial write |
| `await connection.flush()` | Flush locally buffered output |
| `await connection.shutdown_read()` / `shutdown_write()` | Shut down one direction |
| `await connection.close()` | Close both directions |
| `connection.into_split()` | Consume the wrapper and return directional halves |
| `connection.info()` | Obtain a `ConnectionInfo` diagnostic snapshot |

`read()` and `read_bytes()` alias receive. `write_bytes()` aliases the partial
write operation; `write_all()` and `write_all_bytes()` alias complete-buffer
send. Timed reads and complete writes are listed in the timeout reference above.

Connection diagnostics include `id`, `name`, `endpoint`, optional
`local_endpoint` and `peer_endpoint`, `at_eof()`, `peer_sent_eof()`,
`is_read_shutdown()`, `is_write_shutdown()`, and `is_closed()`.

`is_started()` and `is_active()` report a non-closed state. `is_available()` also
checks the pause flag. These are local state checks, not peer health probes or
proof that the next I/O operation will succeed. Diagnostic identifiers are
process-local, not persistent session IDs or authenticated identities.

#### Split connections and pause behavior

`into_split()` is synchronous and consumes a raw Python connection wrapper. It
raises `RuntimeError` if the wrapper was already split, is being upgraded, has
already become secure, or an asynchronous operation still holds the raw
connection. A secure session cannot be converted into raw directional halves.

`ReadHalf` provides `receive()`, `read()`, and `shutdown_read()`. `WriteHalf`
provides `send()`, `write()`, `flush()`, and `shutdown_write()`. Both expose `id`
and `info()` and share the native connection state.

Pausing a server or connection is a local admission check, not an OS-level
suspension or a guarantee that already running I/O is interrupted. Resume with
`resume()`; do not use pause as a security boundary or a replacement for shutdown.

#### Convenience namespace and compatibility

`LocalTransport.start()` and `LocalTransport.connect()` are static convenience
factories. `LocalTransport` is a concrete namespace, not a transport trait.

Python retains `Listener = Server`, `Stream = Connection`, and
`Client = Connection`. These aliases do not introduce separate state machines.

### Python secure API

Import the public facade and identity helpers from `taunicorn.security`. Create
sessions through the asynchronous factories, not by calling `SecureConnection()`.

| API | Purpose |
| --- | --- |
| `await SecureConnection.client(connection, *, identity_private_key, server_identity_public_key)` | Transfer an unsplit connection and perform the existing Rust client handshake |
| `await SecureConnection.server(connection, *, client_identity_public_key, identity_private_key)` | Transfer an unsplit connection and perform the existing Rust server handshake |
| `await secure.send(plaintext)` | Encrypt and send one `bytes` value |
| `await secure.receive()` | Authenticate and decrypt one message, returning `bytes` |
| `await secure.send_msgpack(value)` / `receive_msgpack()` | Serialize or deserialize with the optional Python `msgpack` package |
| `await secure.close()` | Make the secure session terminal and close its native transport |
| `secure.is_closed()` | Report native closure or binding-level terminal cancellation state |
| `secure.connection` | Return the original Python connection for diagnostics and closing, not raw I/O |
| `generate_identity_private_key()` | Return a fresh raw 32-byte private identity key from Rust |
| `identity_public_key(identity_private_key)` | Derive the raw 32-byte public identity key in Rust |

The public async methods are Python coroutines; their native counterparts are
awaitable-returning bindings. Private Python cryptographic state and exact
legacy exception messages are not part of the compatibility contract. The
unchanged Rust counters also determine sequence-exhaustion behavior.

### Rust transport API

The primary public types are `Endpoint`, `Server`, `Connection`, `ReadHalf`,
`WriteHalf`, `ReceiveResult`, `ServerInfo`, `ConnectionInfo`, and
`LocalTransport`. Operations return `anyhow::Result` where fallible.

`Connection::receive(&mut buffer)` returns `ReceiveResult`, whereas
`Connection::read_timeout(max_bytes, duration)` returns an allocated byte vector.

An empty vector from a non-zero timed read indicates EOF.

`Connection::into_split(self)` consumes the Rust connection and returns concrete
read and write halves. The ordinary API does not require custom transport traits.

The compatibility aliases are `SocketListener`, `SocketStream`, and
`SocketClient`.

### Queue API

Both Python queue types expose the same lifecycle and send/receive operations:

| Operation | Behavior |
| --- | --- |
| `await queue.send(item)` | Send through the async bridge; bounded queues wait for free capacity |
| `await queue.recv()` | Wait for the next item, or fail after closure and drain |
| `queue.try_send(item)` | Attempt an immediate send without requiring an event loop |
| `queue.try_recv()` | Attempt an immediate receive without waiting for an item or receiver lock |
| `queue.close()` | Reject new sends, wake waiters, and preserve buffered items |
| `queue.is_closed()` | Report whether sending is closed; not whether the buffer is empty |

`BoundedQueue(capacity)` requires a supported capacity of at least one.

Unsupported capacities raise `ValueError`. `capacity()` reports currently
available slots; `max_capacity()` reports the construction-time limit. These
values are not memory-size limits.

`UnboundedQueue()` has no fixed channel-capacity limit. Applications must bound
producer behavior when uncontrolled memory growth is unacceptable.

Async queue methods return `asyncio.Future` objects and require a running event
loop. Non-blocking receive can raise `QueueBusy` when another operation holds
the receiver lock. Queue closure is idempotent; `QueueClosed` indicates a closed
sending side or, for receives, a queue that is both closed and drained.

### Python exceptions

Catch errors according to the operation, rather than treating all failures as
transport disconnects.

| Condition | Exception |
| --- | --- |
| Invalid endpoint, timeout, capacity, key length, or oversized outgoing secure plaintext | `ValueError` |
| Invalid endpoint argument type, or non-`bytes` secure key/payload | `TypeError` |
| Operation timeout | `TimeoutError` |
| Closed, stopped, or directionally shut-down transport | `ConnectionError` |
| Native secure identity, signature, framing, sequence, or authentication failure | `ConnectionError`, apart from the EOF, sequence-exhaustion, and preserved I/O cases below |
| EOF while assembling a secure frame | `EOFError` |
| Secure send or receive sequence exhausted | `OverflowError` |
| Paused raw transport | `BlockingIOError`; an error returned from native secure I/O is mapped by the secure binding instead |
| Bounded queue has no available slot | `QueueFull` (`BlockingIOError`) |
| No immediately available queue item | `QueueEmpty` (`BlockingIOError`) |
| Queue receiver lock is held | `QueueBusy` (`BlockingIOError`) |
| Queue sending is closed, or a closed queue is drained | `QueueClosed` (`RuntimeError`) |
| Preserved native `std::io::Error`, or failure to generate an identity key | `OSError` |
| Consumed/busy wrapper, raw I/O after secure upgrade, or invalid split/upgrade attempt | `RuntimeError` |
| Secure session already closed or made terminal by cancellation/failure | `ConnectionError` |
| Missing optional `msgpack` package | `ModuleNotFoundError` |
| Other unclassified transport runtime failure | `RuntimeError` |
| Panic propagated by the Rust async bridge | `RustPanic` (`BaseException`) |

`RustPanic` is the async bridge's exported panic type, not a separate lookalike
exception. As a `BaseException` subclass, it is not covered by a normal
`except Exception` handler. MessagePack decoding may additionally raise the
serializer's own errors.

The core still returns `anyhow::Error`. The secure binding recognizes the
uploaded core's EOF and sequence-exhaustion messages and preserves contained
`std::io::Error` values; other native security errors become `ConnectionError`.

A paused-transport error returned during native secure I/O is therefore not a
recoverable Python `BlockingIOError`: it makes that secure session terminal.

Check these mappings when changing the core's error messages. Argument/type
checks and outgoing-size validation happen before native secure I/O; they are
not themselves terminal protocol failures.

### Secure protocol

#### Handshake and key derivation

Each session authenticates persistent Ed25519 identities against explicit peer
pins and establishes fresh X25519 ephemeral key material.

1. The client signs its identity, the expected server identity, its ephemeral public key, and a fresh handshake nonce.
2. The server verifies the client pin and signature, then signs both identities, both ephemeral public keys, and both nonces. The client verifies the server pin and signature.
3. Both sides validate the X25519 exchange and derive independent directional keys and nonce prefixes with HKDF-SHA256, using the handshake transcript hash as salt.
4. Encrypted server and client `finished` messages confirm the derived keys before the constructors return a session.

Application messages use ChaCha20-Poly1305. Associated data binds the protocol
version, traffic direction, sequence number, and handshake transcript. Each
direction has its own lock, key, nonce prefix, and sequence counter.

The design uses ephemeral rather than identity-derived traffic keys to support
forward secrecy. This is a protocol property, not a guarantee of secure key
storage, memory erasure, or resistance to a compromised endpoint.

#### Encoding and size limits

The shared protocol uses version `2`. `TNE2` is a domain-separation marker used
in signatures, transcript hashing, and associated data; it is not a magic
prefix prepended to every transmitted frame.

Handshake and encrypted frames share an unsigned 32-bit big-endian length
prefix. An encrypted application frame has this layout:

```text
uint32_be frame_body_length
uint8     protocol_version
uint64_be sequence_number
bytes     ciphertext || 16-byte authentication tag
```

The length counts the frame body, excluding the four-byte outer prefix.

| Quantity | Value |
| --- | --- |
| Identity private / public key | 32 bytes each |
| Client or server hello body | 162 bytes |
| Maximum frame body | 67,108,864 bytes (64 MiB) |
| Encrypted body overhead | 25 bytes: version + sequence + authentication tag |
| Maximum application plaintext | 67,108,839 bytes (64 MiB minus 25 bytes) |
| Total per-message wire overhead | 29 bytes, including the outer length prefix |
| AEAD nonce | 12 bytes: four-byte derived prefix + eight-byte sequence |

An empty application plaintext is valid because its encrypted body still
contains protocol metadata and an authentication tag. An empty outer frame body
is invalid.

The `finished` messages consume sequence zero in each direction; application
messages begin at sequence one. Receivers require the exact next sequence
number. There is no automatic rekey or counter wraparound; establish a new
session before the sending sequence space is exhausted.

The frame-size constant is not a per-session configuration option. It is also
not a total memory bound: frame assembly, encryption, copying, and serialization
may allocate additional buffers.

The Python binding rejects plaintext above 67,108,839 bytes before native
encryption, raising `ValueError` without consuming a sequence number or making
the session terminal. The unchanged Rust core still validates outgoing frame
size after encryption. Python's preflight check does not lower the native
inbound allocation limit, and MessagePack serialization can allocate its output
before that outgoing check runs.

#### Cross-language compatibility

Python and Rust now use the same Rust handshake layouts, key schedule, and
encrypted-frame encoding. The binding introduces no new wire protocol version:
it delegates to the existing version-2 implementation. The protocol still
targets Python/Python, Rust/Rust, and both mixed-language client/server
combinations.

Compatibility requires matching protocol versions, reciprocal identity pins,
and compatible application encodings. MessagePack remains a separate Python
serialization layer, not a reason to assume identical Python/Rust object
mappings. The handshake does not depend on MessagePack.

The former Python cryptography implementation used the corresponding version-2
wire format. Keep both client/server directions against that legacy implementation
in migration tests when supporting older peers. Shared source code and matching
formats do not replace interoperability tests against the exact deployed
builds, and this migration does not promise identical private state or every
legacy error/edge-case behavior.

## Limitations and known risks

### Transport and delivery guarantees

The raw transport does not provide authentication, encryption, message framing,
or application-level acknowledgements. Neither layer provides reconnect,
durable delivery, deduplication, or exactly-once processing.

Secure sequence checks protect message order within a session. They do not
prevent an application from resubmitting the same operation in a new session.

A successful secure send remains local transport progress, not remote commit.

### Secure failure recovery

The existing Rust protocol rejects invalid identities, signatures, key
confirmation, frames, sequence numbers, and authentication tags. The new Python
binding adds a terminal-session policy around that unchanged implementation:
a native handshake/message failure or observed Python cancellation prevents
further secure I/O on the affected session. After a successful upgrade,
transport cleanup is scheduled on Tokio; invalidation is not a guarantee that
OS resources have already been released when the error reaches Python.

A cancelled secure receive may already have consumed part of a frame. A failed
or cancelled secure send may leave transport progress and sequence state
ambiguous. Separate per-direction binding guards prevent subsequent secure
operations from reusing that session after the failure is observed while
retaining concurrent send/receive during normal operation.

Argument validation before ownership transfer, invalid payload types, and
outgoing plaintext-size rejection happen before native secure I/O. They do not
by themselves poison a live session. MessagePack serialization/deserialization
errors are also separate from native authentication or framing failures; a
decode error after successful authentication alone does not automatically close
the session. Applications must still apply their own invalid-message policy.

A failed or cancelled handshake after ownership transfer leaves the original
Python wrapper unavailable for raw I/O. A failure to acquire exclusive ownership
before starting the handshake instead leaves the raw connection in place.

Calling `close()` requests cleanup in either supported state; it does not make
a failed session reusable.

Direct Rust callers do not receive these Python-binding guards. The unchanged
Rust secure API has no universal permanently-invalid-session mechanism for
validation errors or dropped futures. Its callers remain responsible for
closing and discarding affected sessions.

> [!IMPORTANT]
> Close and discard the connection after a native secure-operation failure or
> cancellation, including a handshake failure after ownership transfer. Do not
> retry on the same secure session, bypass the secure layer, or fall back
> automatically to plaintext. Local validation and serialization errors are
> separate cases, not evidence of a failed cryptographic session.

### Resource and availability limits

The common frame reader accepts bodies up to the 64-MiB ceiling before
hello-specific length validation. The constructors do not expose a smaller
handshake receive limit or a built-in deadline.

A post-decryption application size check does not prevent the earlier frame
allocation. Deployments that require a lower pre-allocation limit need that
limit enforced in the framing implementation, not merely in a message handler.

Unbounded queues, slow consumers, large cryptographic operations, and unlimited
connection handlers can also consume excessive memory or execution time. Secure
handshake and message cryptography now run in Rust/Tokio rather than in the
Python protocol implementation; that does not impose a CPU or memory budget.

Python-side MessagePack serialization/deserialization and native-boundary buffer
conversions can still delay the event loop. Bound work and payload sizes rather
than assuming that a native binding makes these costs disappear.

### Security boundary

The secure layer protects authenticated message contents, not traffic metadata
or the host itself. Handshake identities, frame lengths, protocol fields, and
traffic timing are not hidden. The outer length prefix is not directly included
in the AEAD associated data.

There is no authenticated close notification, PKI discovery, automatic key
rotation, multi-peer trust directory, application authorization policy, or
protection against a compromised local process with access to its plaintext or
private keys. This README does not claim an independent cryptographic audit.

## Troubleshooting

| Symptom | What to check | Action |
| --- | --- | --- |
| Native module cannot be imported | Active interpreter, OS/CPU target, ABI tag, runtime libraries, and source build output | Install the matching wheel into the running environment or rebuild for that target; `abi3` is not a universal platform binary |
| `taunicorn.security` import fails because native security names are missing | Facade and extension from different builds, or an old extension | Rebuild/reinstall the integrated Python package; installing `cryptography` does not add missing native exports |
| `send_msgpack()` / `receive_msgpack()` raises `ModuleNotFoundError` | Optional `msgpack` dependency | Install `msgpack` in the active environment; byte-oriented secure methods do not require it |
| Installation still requests `cryptography` | Installed release metadata, lockfiles, legacy module, or another dependency | Remove this security layer's obsolete requirement during integration only if no other code needs it; rebuild and inspect the resulting wheel metadata |
| Endpoint creation or connection fails | Name, existing listener, backend permissions, server lifecycle | Inspect the OS error and confirm the intended listener is running |
| Raw receive returns fewer bytes than expected | Stream chunking, not message boundaries | Accumulate a bounded application record or use the secure message API |
| Raw receive returns `b""` | Requested size and observed peer EOF | Distinguish a zero-size read from EOF; check `at_eof()` |
| `BlockingIOError` during transport I/O | Local pause state | Resume the appropriate object or correct the lifecycle logic |
| `into_split()` or secure upgrade raises `RuntimeError` | Existing split/upgrade/secure state or in-flight raw operation | Finish the raw operation before transferring ownership; do not split or re-upgrade a secure session |
| Raw I/O raises `RuntimeError` after handshake | Connection is already owned by the secure layer | Use `secure.send()` / `secure.receive()`; keep the original connection only for diagnostics and closing |
| Secure I/O raises a closed/cancelled-session error | Earlier cancellation or native protocol/I/O failure | Close and discard the session; establish a new connection and handshake |
| Secure handshake stalls | Opposite role, protocol version, peer progress, missing deadline | Bound the handshake and close the raw connection on failure |
| Secure identity or signature check fails | Pinned public key, local identity, key encoding | Correct provisioning through a trusted path; do not disable verification |
| Secure sequence, tag, or frame error | Mixed raw/secure I/O, corruption, incompatible version, interrupted operation | Close the session and investigate before establishing another |
| Queue operation cannot proceed immediately | Full buffer, empty buffer, receiver lock, or closed state | Handle `QueueFull`, `QueueEmpty`, `QueueBusy`, or `QueueClosed` separately |
| Peer EOF is mistaken for completed work | Missing application completion acknowledgement | Define an explicit application-level success response |

`info()` provides immutable diagnostic snapshots, not live views. Capture them
alongside operation context when investigating lifecycle failures. Do not log
private keys or sensitive payloads as part of troubleshooting.

## Security and operations

### Identity and access control

Treat local IPC as a trust boundary. Use deliberate endpoint names and
appropriate OS access controls even when payload encryption is enabled.

Protect persistent Ed25519 private keys and distribute public-key pins through
an authenticated administrative path.

Peer authentication proves possession of the pinned identity key. It does not
authorize every request from that identity. Apply application authorization and
validate all decoded data before performing an operation.

### Deadlines and resource budgets

Define deadlines for connection establishment, handshakes, application
exchanges, and shutdown. Limit concurrent handlers and outstanding work. Use
bounded queues when channel capacity must apply backpressure, and account for
the size of each queued object separately.

Set application message limits below the protocol ceiling when appropriate.

Where a threat model requires strict allocation limits, enforce them before
frame allocation. Do not represent an after-the-fact payload check as an
inbound memory limit.

### Shutdown and observability

Stop accepting new connections before shutting down connection handlers. Track
accepted connections and tasks explicitly; stopping the listener is not a
connection drain. Finish or cancel handlers according to application policy,
then close their transports.

Log operation type, endpoint, local diagnostic connection ID, duration, and
failure category where useful. Avoid private identity keys, traffic keys,
plaintext secrets, and complete sensitive payloads. Treat ambiguous delivery
as an application recovery problem rather than an automatic transport retry.

## Development and releases

### Repository responsibilities

| Location | Responsibility |
| --- | --- |
| `crates/taunicorn/` | Existing native transport and secure protocol; unchanged by the Python security migration |
| `crates/taunicorn-python/` | PyO3 transport/security bindings, Python-side ownership policy, and Rust-backed Python channels |
| `python/taunicorn/` | Public package, thin security coroutine facade, optional MessagePack helpers, and typing assets |
| `python/tests/` | Python tests and native integration coverage |
| `docs/` | Supporting documentation |
| `.github/workflows/` | CI, security checks, and publishing workflows |
| Root `Cargo.toml` and `pyproject.toml` | Workspace, dependency, and packaging configuration |

### Integrating the Python security replacement

The integration package supplies binding code and Python package changes, not a
replacement Rust core. For this README's root-level `python/` layout, place the
new native module at `crates/taunicorn-python/src/security_binding.rs` and the
replacement facade at `python/taunicorn/security.py`. Merge the binding ownership
and module-registration changes into `crates/taunicorn-python/src/lib.rs`.

Keep the existing `channel.rs` and all of `crates/taunicorn/`.

Merge the native security declarations into `python/taunicorn/_taunicorn.pyi`.

When exporting the public secure API from `python/taunicorn/__init__.py`, import
it from `.security` after any native wildcard import so the coroutine facade is
not overwritten by the native awaitable-returning class. Retain other exports
and the existing `py.typed` marker.

The binding's `tokio-util` dependency enables `rt` for its cancellation-token
guards. Preserve the supplied PyO3 ABI configuration and the existing Rust core
manifest. Merge packaging changes instead of replacing unrelated dependencies
or metadata. Remove `cryptography` from Python runtime requirements only when
no other package code needs it; optionally declare `msgpack` as an extra.

Update applicable lockfiles in a controlled development change before using
`--locked` builds.

The integration archive's example puts Python sources and `pyproject.toml`
beside the binding crate. That is not the root-level layout documented here:
copy its Python assets into `python/taunicorn/` and retain the root Maturin paths
shown below. Copy its supplied tests into `python/tests/` when using this
README's test commands. Do not create a second Python package tree by accident.

### Local checks

From the workspace root, check formatting, compile the native crate, run Clippy,
and execute native tests:

```bash
cargo fmt --all -- --check
cargo check -p taunicorn --all-targets --locked
cargo clippy -p taunicorn --all-targets --locked -- -D warnings
cargo test -p taunicorn --all-targets --locked
```

Check the binding crate separately:

```bash
cargo check -p taunicorn-python --all-targets --locked
cargo clippy -p taunicorn-python --all-targets --locked -- -D warnings
```

Run the Python suite in the configured development environment, then build the
Python distributions:

```bash
uv run pytest python/tests
uv build
```

To explicitly rebuild the extension after integrating the binding, use Maturin
from the repository root in an activated development environment with Maturin,
pytest, and any required test extras installed:

```bash
maturin develop --release
python -m pytest -q python/tests
maturin build --release --locked --out dist
```

The last command assumes the workspace lockfile already includes the integrated
binding dependencies. A source-only Python test against an older installed
extension is not validation of the new binding. Run native integration tests
against the extension just built, then test the wheel in a clean environment.

### Packaging and typing

Maturin is the documented PEP 517 build backend. Its binding configuration points
to the dedicated PyO3 crate and the Python package directory. The relevant
binding metadata is:

```toml
[tool.maturin]
bindings = "pyo3"
manifest-path = "crates/taunicorn-python/Cargo.toml"
python-source = "python"
module-name = "taunicorn._taunicorn"
```

Keep build-system version requirements in `pyproject.toml` rather than copying
independent pins into this README.

The binding crate builds the `_taunicorn` `cdylib`, and Maturin packages it as
`taunicorn._taunicorn` alongside the public Python modules. Cryptographic
operations come from its existing `taunicorn` Rust dependency rather than a
separate Python `cryptography` dependency.

The retained PyO3 `abi3-py310` feature targets a `cp310-abi3` wheel for compatible
GIL-enabled CPython versions from 3.10. It does not cover free-threaded CPython
with that wheel and does not remove platform differences. Linux/glibc,
Linux/musl, Windows, macOS, and their CPU architectures require appropriate
build targets. Produce Linux release wheels with the intended Manylinux or
Musllinux compatibility and test them on the supported targets; a successful
local build is not proof of wheel portability.

Keep the supported interpreter and platform matrix in the actual manifests and
CI configuration. Build and install the extension and facade together. Inspect
wheel contents and runtime dependency metadata to confirm that this security
layer no longer declares `cryptography`; other dependencies may still require
it. Do not remove an unrelated dependency merely to make that check pass.

The Python package includes `_taunicorn.pyi` and `py.typed` for PEP 561 typing.

The native stub now also describes the secure class and identity helpers, in
addition to awaitable transport operations, synchronous state methods, split
halves, queues, exceptions, and compatibility aliases. The public `security.py`
facade retains coroutine signatures and optional MessagePack helpers; the native
secure class itself exposes byte-oriented methods, not Python-object MessagePack
methods.

### Native security integration status

The supplied integration report records 13 passing Python-facade tests using a
simulated native module. Those checks cover delegation, keyword arguments,
connection-object identity, coroutine/task behavior, byte payloads, optional
MessagePack handling, and cancellation forwarding. Python 3.10 syntax, TOML
parsing, and application of the binding patch to the uploaded source were also
reported as checked.

The native integration-test module was skipped when that package was prepared:
no Rust compiler or compiled extension was available. No Rust/PyO3 compilation,
linking, native handshake, legacy-Python interoperability run, Maturin wheel
build, or target ABI/platform matrix was completed as part of those checks.

These are source-integration results, not a tested release or a cryptographic
audit. Updating this README does not change that validation status.

Before release, compile the binding and run the supplied native tests against
the built extension. Exercise both client/server roles, wrong identity pins,
empty and binary messages, concurrent send/receive, raw-I/O rejection after
upgrade, cancellation, closing, and size/error boundaries. Test both roles
against the legacy Python implementation when supporting older peers; that
comparison needs `cryptography` only in its isolated legacy-test environment.

Finally, install and exercise each target wheel in clean environments, including
a byte-only environment without `cryptography` or `msgpack` and a separate
MessagePack-enabled environment. Recorded facade checks do not substitute for
any of those native or distribution checks.

### CI and publishing

The documented workflow responsibilities are separated by concern:

| Workflow | Responsibility |
| --- | --- |
| `rust-ci.yml` | Formatting, compilation, Clippy, and native transport tests |
| `python-ci.yml` | Python version/platform checks, wheel installation, integration tests, and source-distribution checks |
| `security.yml` | Rust/Python dependency audits and dependency-change checks |
| `publish-python.yaml` | Python version validation, distribution builds, smoke tests, attestations, and PyPI publishing |
| `publish-rust.yml` | Cargo publish dry run and explicitly gated crates.io publishing |

The documented Python matrix covers Python 3.10–3.14 on Linux and Python 3.14 on
Windows and macOS targets. Native transport tests target Linux, Windows, and
macOS Intel and ARM64. The checked-out workflow files, not this summary, define
the actual matrix and release gates.

Python publishing is documented as release-triggered, with PEP 740 attestations
and OIDC-based PyPI Trusted Publishing through a `pypi` environment.

Rust publishing is separate, explicitly gated, and uses a protected `crates-io`
environment with short-lived registry credentials. Configure repository and
publisher trust for the exact workflow rather than assuming the README enables
publishing by itself.

Dependency maintenance covers GitHub Actions, Cargo, and `uv` through
`.github/dependabot.yml`. Branch selection, update cadence, dependency versions,
and workflow action pins belong in the actual configuration files.

These descriptions are not evidence of a passing CI run, an available release
artifact, or a completed cross-language security test suite. Verify those against
the release being reviewed.

### Contribution scope

Run the applicable local checks before submitting changes. Keep framing, RPC,
routing, persistence, and retry/replay behavior above the raw transport rather
than making them implicit `Connection` semantics.

Changes to wire encoding, cancellation, shutdown, or secure-session state should
include focused tests. Protocol changes also require interoperability coverage
for both Python/Rust client-server directions and failure-path testing.

## License

See `LICENSE`, `LICENSE-APACHE-2.0`, and `LICENSE-MIT` for the applicable terms.
