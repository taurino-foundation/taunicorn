import { randomUUID } from 'node:crypto'
import { mkdtemp, realpath, rm } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'
import test from 'ava'

import { Client, type Connection, Server } from '../javascript/taunicorn/index.js'

test('public API exchanges bytes concurrently in both stream directions', async (t) => {
  const endpoint = `taunicorn-js-test-${process.pid}-${Date.now()}`
  const listening = Server.listen(endpoint)
  t.true(listening instanceof Promise)
  const server = await listening
  const accepting = server.accept()
  let client: Connection | undefined
  let accepted: Connection | undefined

  try {
    client = await Client.connect(endpoint)
    accepted = await accepting

    const clientBytes = Buffer.from([255])
    const serverBytes = Buffer.from([0])
    await Promise.all([client.send(clientBytes), accepted.send(serverBytes)])

    const [receivedByServer, receivedByClient] = await Promise.all([
      accepted.receive(clientBytes.length),
      client.receive(serverBytes.length),
    ])

    t.deepEqual(receivedByServer, clientBytes)
    t.deepEqual(receivedByClient, serverBytes)
  } finally {
    await server.stop()
    await Promise.allSettled([accepting, client?.close(), accepted?.close()])
  }
})

test.serial('private factories exchange bytes through standard accept', async (t) => {
  const dir = await mkdtemp(join(await realpath(homedir()), 'tu-'))
  const previous = process.env.XDG_RUNTIME_DIR
  const mask = process.umask(0o077)
  process.env.XDG_RUNTIME_DIR = dir
  const endpoint = `tu-${randomUUID().slice(0, 12)}`
  try {
    let server: Server
    try {
      server = await Server.listenPrivate(endpoint)
    } catch (error) {
      if (
        ['linux', 'win32'].includes(process.platform) ||
        !(error instanceof Error) ||
        error.message !== 'UnsupportedPlatform'
      )
        throw error
      await t.throwsAsync(Client.connectPrivate(endpoint), { message: 'UnsupportedPlatform' })
      t.log('Core cannot verify this runtime directory’s ACL')
      return
    }
    try {
      const [client, peer] = await Promise.all([Client.connectPrivate(endpoint), server.accept()])
      try {
        await client.send(Buffer.from([255]))
        t.deepEqual(await peer.receive(1), Buffer.from([255]))
        await peer.send(Buffer.from([0]))
        t.deepEqual(await client.receive(1), Buffer.from([0]))
      } finally {
        await Promise.all([client.close(), peer.close()])
      }
    } finally {
      await server.stop()
    }
  } finally {
    process.umask(mask)
    if (previous === undefined) delete process.env.XDG_RUNTIME_DIR
    else process.env.XDG_RUNTIME_DIR = previous
    await rm(dir, { recursive: true, force: true })
  }
})

test('private factories reject invalid service names', async (t) => {
  await t.throwsAsync(Server.listenPrivate('invalid service name'), { message: /InvalidEndpoint/ })
  await t.throwsAsync(Client.connectPrivate('invalid service name'), { message: /InvalidEndpoint/ })
})

for (const method of ['receive', 'readBytes', 'readTimeout', 'split'] as const) {
  test(`raw ${method} preserves bytes across bounded chunks`, async (t) => {
    const endpoint = `tu-${randomUUID().slice(0, 12)}`
    const server = await Server.listen(endpoint)
    try {
      const [client, peer] = await Promise.all([Client.connect(endpoint), server.accept()])
      try {
        const reader = method === 'split' ? peer.intoSplit().read : peer
        const read = (n: number) =>
          method === 'readTimeout'
            ? peer.readTimeout(n, 10000)
            : method === 'readBytes'
              ? peer.readBytes(n)
              : reader.receive(n)
        t.deepEqual(await read(0), Buffer.alloc(0))
        const payload = Buffer.alloc(256 * 1024)
        for (let i = 0; i < payload.length; i++) payload[i] = i & 255
        async function drain() {
          const chunks = []
          for (let n = 0; n < payload.length;) {
            const chunk = await read(payload.length)
            if (!chunk?.length || chunk.length > 65536) throw new Error('invalid raw chunk size')
            chunks.push(chunk)
            n += chunk.length
          }
          t.deepEqual(Buffer.concat(chunks), payload)
        }
        await Promise.all([client.send(payload), drain()])
      } finally {
        await Promise.all([client.close(), peer.close()])
      }
    } finally {
      await server.stop()
    }
  })
}
