import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { Client, Server } from 'taunicorn'

const require = createRequire(import.meta.url)
const [abi] = process.argv.slice(2)
const meta = require('taunicorn/package.json')
const platform = `${meta.name}-${abi}`
assert.equal(meta.optionalDependencies[platform], meta.version)
assert.equal(Object.keys(meta.optionalDependencies).length, 4)
assert.equal(require(platform).Client, Client)
assert.equal(require('taunicorn').Server, Server)
assert.ok(require.resolve(platform).endsWith(`.${abi}.node`))

const endpoint = `taunicorn-stage-${process.pid}-${Date.now()}`
const server = await Server.listen(endpoint)
async function echo() {
  const stream = await server.acceptTimeout(15000)
  try {
    assert.deepEqual(await stream.receive(1), Buffer.from([255]))
    await stream.send(Buffer.from([0]))
  } finally {
    await stream.close()
  }
}
async function client() {
  const stream = await Client.connect(endpoint)
  try {
    await stream.send(Buffer.from([255]))
    assert.deepEqual(await stream.receive(1), Buffer.from([0]))
  } finally {
    await stream.close()
  }
}
try {
  await Promise.all([echo(), client()])
  console.log(`${platform}@${meta.version}: CJS, ESM and Node/Node IPC passed`)
} finally {
  await server.stop()
}
