import { Connection } from 'taunicorn'

const ENDPOINT = 'taunicorn-js-py-bridge'

class RpcClient {
  constructor(conn) {
    this.conn = conn
    this.buffer = Buffer.alloc(0)
    this.nextId = 1
    this.pending = new Map()
    this.closed = false
    this.readLoopPromise = this.readLoop() // Promise merken
  }

  async readLoop() {
    try {
      while (!this.closed) {
        const chunk = await this.conn.receive(4096)
        if (chunk === null) {
          console.log('[js-client] server sent EOF')
          break
        }
        if (chunk.length === 0) {
          // leerer Buffer ist kein EOF – weiter warten
          continue
        }
        console.log('[js-client] recv:', JSON.stringify(chunk.toString()))

        this.buffer = Buffer.concat([this.buffer, chunk])

        let idx
        while ((idx = this.buffer.indexOf(0x0a)) !== -1) {
          if (idx > 8192) throw new Error('record too long')
          const line = this.buffer.subarray(0, idx).toString()
          this.buffer = this.buffer.subarray(idx + 1)
          if (!line.trim()) continue

          let resp
          try {
            resp = JSON.parse(line)
          } catch (e) {
            console.error('[js-client] invalid response:', line)
            continue
          }

          const pending = this.pending.get(resp.id)
          if (pending) {
            this.pending.delete(resp.id)
            pending.resolve(resp.result)
          }
        }
        if (this.buffer.length > 8192) throw new Error('record too long')
      }
    } catch (err) {
      if (!this.closed) {
        console.error('[js-client] readLoop error:', err)
      }
    } finally {
      for (const [, pending] of this.pending) {
        pending.reject(new Error('connection closed before response'))
      }
      this.pending.clear()
      await this.close()
    }
  }

  call(method, params = {}) {
    if (this.closed) {
      return Promise.reject(new Error('client is closed'))
    }
    const id = this.nextId++
    const req = JSON.stringify({ id, method, params })
    console.log('[js-client] send:', req)

    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      this.conn.send(Buffer.from(req + '\n')).catch((err) => {
        this.pending.delete(id)
        reject(err)
      })
    })
  }

  async close() {
    if (this.closed) return
    this.closed = true
    try {
      await this.conn.close()
    } catch (err) {
      console.error('[js-client] close error:', err)
    }
  }

  // Warten bis der readLoop beendet ist (wichtig für sauberes Prozess-Ende)
  async waitForReader() {
    await this.readLoopPromise
  }
}

async function main() {
  console.log('[js-client] connecting to Python server ...')
  const conn = await Connection.connect(ENDPOINT)
  console.log('[js-client] connected')
  const rpc = new RpcClient(conn)

  console.log('[js-client] calling add ...')
  const sum = await rpc.call('add', { a: 20, b: 22 })
  console.log('[js-client] add ->', sum)

  console.log('[js-client] calling greet ...')
  const greeting = await rpc.call('greet', { name: 'example' })
  console.log('[js-client] greet ->', greeting)

  console.log('[js-client] calling time ...')
  const t = await rpc.call('time')
  console.log('[js-client] time ->', t)

  console.log('[js-client] calling quit ...')
  await rpc.call('quit')

  await rpc.close()
  await rpc.waitForReader() // wichtig: warten bis readLoop fertig ist

  console.log('[js-client] done ✅')
}

main().catch((err) => {
  console.error('[js-client] failed:', err) // echten Fehler zeigen
  process.exitCode = 1
})
