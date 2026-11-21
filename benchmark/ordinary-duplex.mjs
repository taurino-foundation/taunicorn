import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { performance } from 'node:perf_hooks'
import { Connection, Server } from '../javascript/taunicorn/index.js'

async function readExactly(conn, size) {
  const bytes = Buffer.alloc(size)
  for (let at = 0; at < size;) {
    const chunk = await conn.receive(size - at)
    assert(chunk && chunk.length > 0 && chunk.length <= size - at, 'short stream or invalid read')
    at += chunk.copy(bytes, at)
  }
  return bytes
}

const watchdog = setTimeout(() => { console.error('ordinary duplex timed out'); process.exit(1) }, 15000)
const name = `tu-bench-${randomUUID()}`
const server = await Server.listen(name)
const connect = () => Promise.all([Connection.connect(name), server.accept()])
const ping = Buffer.alloc(64, 0xa5)
const up = Buffer.alloc(4096, 0x51)
const down = Buffer.alloc(4096, 0xa2)
const [client, peer] = await connect()
const samples = []
for (let i = 0; i < 120; i++) {
  ping.writeUInt32LE(i)
  const start = performance.now()
  await client.send(ping)
  const request = await readExactly(peer, ping.length)
  await peer.send(request)
  const echo = await readExactly(client, ping.length)
  const elapsed = performance.now() - start
  assert.deepEqual(echo, ping)
  if (i >= 20) samples.push(elapsed)
}
samples.sort((a, b) => a - b)
const start = performance.now()
for (let i = 0; i < 32; i++) {
  up.writeUInt32LE(i)
  down.writeUInt32LE(i)
  const [a, b] = await Promise.all([readExactly(peer, up.length), readExactly(client, down.length),
    client.send(up), peer.send(down)])
  assert.deepEqual(a, up)
  assert.deepEqual(b, down)
}
const seconds = (performance.now() - start) / 1000
await Promise.all([client.close(), peer.close()])
const [next, accepted] = await connect()
await next.send(Buffer.from('new'))
assert.deepEqual(await readExactly(accepted, 3), Buffer.from('new'))
await Promise.all([next.close(), accepted.close(), server.stop()])
clearTimeout(watchdog)
console.log(JSON.stringify({ rtt_p50_ms: samples[49], rtt_p99_ms: samples[98],
  aggregate_MiB_s: 32 * (up.length + down.length) / 2 ** 20 / seconds,
  peak_process_RSS_KiB: process.resourceUsage().maxRSS, reconnect: 'ok' }))
