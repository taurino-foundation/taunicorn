// benchmark/bench.ts
import { Bench } from 'tinybench'

import {
  Server,
  Connection,
  SecureConnection,
  generateIdentityPrivateKey,
  identityPublicKey,
} from '../javascript/taunicorn/index.js'

const ENDPOINT = 'taunicorn-bench'
const ENDPOINT_SECURE = 'taunicorn-bench-secure'

const SMALL = Buffer.alloc(64) // 64 B — Latency
const MEDIUM = Buffer.alloc(64 * 1024) // 64 KB
const LARGE = Buffer.alloc(1024 * 1024) // 1 MB — Throughput

// ─── Setup: Roh-Verbindung ───────────────────────────────────────────
const server = Server.start(ENDPOINT)
const connPromise = Connection.connect(ENDPOINT)
const acceptedConn = await server.accept()
const conn = await connPromise

// ─── Setup: Secure-Verbindung ────────────────────────────────────────
const serverSeed = generateIdentityPrivateKey()
const clientSeed = generateIdentityPrivateKey()
const serverPub = identityPublicKey(serverSeed) // Client pinnt den Server
const clientPub = identityPublicKey(clientSeed) // Server pinnt den Client

const secureServer = Server.start(ENDPOINT_SECURE)
const secureConnPromise = SecureConnection.connect(ENDPOINT_SECURE, clientSeed, serverPub)
const secureServerConn = await secureServer.acceptSecure(serverSeed, clientPub)
const secureConn = await secureConnPromise

// ─── Benchmarks ──────────────────────────────────────────────────────
const b = new Bench()

// Latency: klein senden + auf Echo warten
b.add('IPC ping-pong 64B', async () => {
  await conn.send(SMALL)
  await acceptedConn.receive(4096)
})

b.add('IPC send 64KB', async () => {
  await conn.send(MEDIUM)
  await acceptedConn.receive(128 * 1024)
})

b.add('IPC send 1MB (throughput)', async () => {
  await conn.send(LARGE)
  await acceptedConn.receive(2 * 1024 * 1024)
})

// possibly-partial write → flush
b.add('IPC write + flush 64KB', async () => {
  await conn.write(MEDIUM)
  await conn.flush()
  await acceptedConn.receive(128 * 1024)
})

// MessagePack-Roundtrip über die sichere Schicht
b.add('secure msgpack roundtrip (small object)', async () => {
  await secureConn.sendMsgpack({ id: 1, payload: SMALL.toString('base64') })
  await secureServerConn.receiveMsgpack()
})

b.add('secure raw send 64KB', async () => {
  await secureConn.send(MEDIUM)
  await secureServerConn.receive(128 * 1024)
})

// Split-Halves (parallele Richtungen)
const halves = conn.intoSplit()
b.add('IPC write via WriteHalf 64KB', async () => {
  await halves.write.send(MEDIUM)
  await acceptedConn.receive(128 * 1024)
})

await b.run()

console.table(b.table())

// ─── Aufräumen ───────────────────────────────────────────────────────
await conn.close()
await acceptedConn.close()
await secureConn.close()
await secureServerConn.close()
await server.stop()
await secureServer.stop()
