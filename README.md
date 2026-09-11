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
`SecureConnection` layer adds authenticated, encrypted messages. Separate
Rust-backed queues provide in-process coordination for Python objects.

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

Python and Rust share the native byte transport. Their secure protocol layers
are separate implementations of the same versioned wire format.

The following paths separate application protocol processing from native I/O:

```text
Python application                         Rust application
  optional Python SecureConnection           optional Rust SecureConnection
  PyO3 / pyo3-async-runtimes                  |
              |                              |
              +-------------+----------------+
                            |
                  Tokio Server / Connection
                            |
                 interprocess local sockets
                            |
                       OS local IPC
```

Python owns its `asyncio` event loop. The async bridge exposes Rust futures as
Python awaitables; native transport I/O executes on Tokio, not directly on the
Python event loop. Connection state, ordering, EOF, and shutdown remain owned by
the Rust transport.

Python secure-session cryptography and MessagePack processing run in the Python
implementation. They are not automatically offloaded to Tokio merely because
the enclosing methods are asynchronous.

The Python queues use Tokio MPSC channels and serialized receiver access. They
hold Python objects within a process; they are not an IPC message broker or a
serialization mechanism.

### Connection lifecycle

`Server.start()` creates a listener. Each successful `accept()` returns an
independent `Connection`; `Connection.connect()` creates the client side.

Stopping the server interrupts pending accepts and releases the listener once
in-flight operations release their references. It does not close already
accepted connections. Applications must manage those connections separately.

A secure session starts with an ordinary connection. The client and server then
perform the secure handshake before exchanging application messages. After the
upgrade, all application traffic must use the secure API.

## Requirements

### Runtime and build requirements

| Use case | Requirements |
| --- | --- |
| Python transport | Python 3.10 or newer, a running `asyncio` loop for asynchronous operations, and the native extension |
| Python secure sessions | Python transport plus `cryptography` and `msgpack` |
| Rust applications | Rust with Cargo and a Tokio runtime |
| Source development | Rust stable, Python 3.10 or newer, `uv`, and the Maturin build configuration |

The Python security module imports both `cryptography` and `msgpack`, including
when only its byte-oriented methods are used. No optional dependency extra is
assumed here.

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

When the environment does not already provide the security module's dependencies,
install them before importing `taunicorn.security`:

```bash
python -m pip install cryptography msgpack
```

Use the public `taunicorn` package for application imports. The native
`taunicorn._taunicorn` extension is an implementation detail.

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

Once established, `secure.send()` accepts plaintext bytes and `secure.receive()`
returns one complete authenticated plaintext message. Do not send or receive
application data through the underlying raw connection after the handshake.

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

Receive the value with `await secure.receive_msgpack()`. In Rust,
`send_msgpack(&value)` requires `serde::Serialize`, and
`receive_msgpack::<T>()` requires `serde::de::DeserializeOwned`. Rust uses named
MessagePack fields; both peers still need a compatible application schema.

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

Do not generalize that binding behavior to Rust future cancellation. The
native `send()` loop has no cancellation guard that closes the connection when
its future is dropped. Prefer the dedicated write-timeout API
for raw timed writes, and explicitly discard failed or cancelled secure sessions.

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

`into_split()` is synchronous and consumes the Python connection wrapper. It
raises `RuntimeError` if the wrapper was already split or an asynchronous
operation still holds a reference to it.

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
| Invalid endpoint, timeout, capacity, or key length | `ValueError` |
| Invalid endpoint argument type | `TypeError` |
| Operation timeout | `TimeoutError` |
| Closed, stopped, or directionally shut-down transport | `ConnectionError` |
| Secure identity, signature, framing, sequence, or authentication failure | Usually `ConnectionError`; key and size validation may raise `ValueError` |
| EOF while assembling a secure frame | `EOFError` |
| Secure send sequence exhausted | `OverflowError` |
| Paused transport | `BlockingIOError` |
| Bounded queue has no available slot | `QueueFull` (`BlockingIOError`) |
| No immediately available queue item | `QueueEmpty` (`BlockingIOError`) |
| Queue receiver lock is held | `QueueBusy` (`BlockingIOError`) |
| Queue sending is closed, or a closed queue is drained | `QueueClosed` (`RuntimeError`) |
| OS I/O failure | `OSError` |
| Consumed wrapper or unclassified runtime failure | `RuntimeError` |
| Panic propagated by the Rust async bridge | `RustPanic` (`BaseException`) |

`RustPanic` is the async bridge's exported panic type, not a separate lookalike
exception. As a `BaseException` subclass, it is not covered by a normal
`except Exception` handler. MessagePack decoding may additionally raise the
serializer's own errors.

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
not a total memory bound: frame assembly, encryption, and serialization may
allocate additional buffers. Outgoing oversize checks occur after encryption.

#### Cross-language compatibility

The Python and Rust sources use matching fixed handshake layouts, key
schedules, and encrypted-frame encodings. The protocol targets Python/Python,
Rust/Rust, and both mixed-language client/server combinations.

Compatibility requires matching protocol versions, reciprocal identity pins,
and compatible application encodings. Matching source formats are not a
substitute for interoperability tests against the exact deployed versions.
The handshake does not depend on MessagePack encoding.

## Limitations and known risks

### Transport and delivery guarantees

The raw transport does not provide authentication, encryption, message framing,
or application-level acknowledgements. Neither layer provides reconnect,
durable delivery, deduplication, or exactly-once processing.

Secure sequence checks protect message order within a session. They do not
prevent an application from resubmitting the same operation in a new session.
A successful secure send remains local transport progress, not remote commit.

### Secure failure recovery

The secure implementations reject invalid identities, signatures, key
confirmation, frames, sequence numbers, and authentication tags. However,
validation errors are propagated without a universal automatic-close or
permanently invalid-session mechanism. Rejection is not the same as guaranteed
resource cleanup.

A cancelled secure receive may already have consumed part of a frame. A failed
or cancelled secure send may leave transport progress and sequence state
ambiguous. Continuing the session is not a supported recovery strategy.

> [!IMPORTANT]
> Close and discard the connection after a failed or cancelled secure operation,
> including handshake failure. Do not retry on the same secure session, bypass
> the secure layer, or fall back automatically to plaintext.

### Resource and availability limits

The common frame reader accepts bodies up to the 64-MiB ceiling before
hello-specific length validation. The constructors do not expose a smaller
handshake receive limit or a built-in deadline.

A post-decryption application size check does not prevent the earlier frame
allocation. Deployments that require a lower pre-allocation limit need that
limit enforced in the framing implementation, not merely in a message handler.

Unbounded queues, slow consumers, large cryptographic operations, and unlimited
connection handlers can also consume excessive memory or execution time. In
Python, synchronous cryptographic and serialization work can delay the event
loop even while native I/O remains Tokio-backed.

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
| Native module cannot be imported | Active Python environment, wheel availability, source build output | Install into the running interpreter's environment; inspect native build errors |
| `taunicorn.security` import fails | `cryptography` and `msgpack` availability | Install both dependencies in the same environment |
| Endpoint creation or connection fails | Name, existing listener, backend permissions, server lifecycle | Inspect the OS error and confirm the intended listener is running |
| Raw receive returns fewer bytes than expected | Stream chunking, not message boundaries | Accumulate a bounded application record or use the secure message API |
| Raw receive returns `b""` | Requested size and observed peer EOF | Distinguish a zero-size read from EOF; check `at_eof()` |
| `BlockingIOError` during transport I/O | Local pause state | Resume the appropriate object or correct the lifecycle logic |
| `into_split()` raises `RuntimeError` | Existing split or in-flight operation holding the wrapper | Finish or cancel and await the operation; do not reuse a consumed wrapper |
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
| `crates/taunicorn/` | Native transport and Rust protocol code |
| `crates/taunicorn-python/` | PyO3 bindings and Rust-backed Python channels |
| `python/taunicorn/` | Public Python package, secure layer, and typing assets |
| `python/tests/` | Python tests and native integration coverage |
| `docs/` | Supporting documentation |
| `.github/workflows/` | CI, security checks, and publishing workflows |
| Root `Cargo.toml` and `pyproject.toml` | Workspace, dependency, and packaging configuration |

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

The Python package includes `_taunicorn.pyi` and `py.typed` for PEP 561 typing.
The native stub describes awaitable transport operations, synchronous state
methods, split halves, queues, queue exceptions, and compatibility aliases.

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
