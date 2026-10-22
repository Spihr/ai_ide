/**
 * 子进程监管：spawn / 帧读写 / 请求超时 / 崩溃检测。
 *
 * 只做「进程 + 帧」这一层，不掺业务语义——它不知道插件是干什么的，
 * 也不知道有「工具」这回事。这样上层的 PluginHost 才可以独立测试。
 *
 * 三条不可让步的约束：
 * 1. 子进程 stdout 只能出现协议帧，任何调试输出走 stderr（否则帧流会被污染）
 * 2. 每个请求必须有超时，否则一个不响应的插件就能挂死整个 Core
 * 3. 单帧超过 1 MiB 直接判协议违规并 kill，不给它把宿主内存撑爆的机会
 */

import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import readline from 'node:readline'
import {
  ErrorCodes,
  MAX_FRAME_BYTES,
  PluginError,
  encodeFrame,
  parseFrame,
  requestFrame
} from './protocol.mjs'

export class PluginProcess extends EventEmitter {
  /**
   * @param {object} opts
   * @param {string} opts.command  可执行文件（demo 用 process.execPath）
   * @param {string[]} opts.args
   * @param {string} opts.cwd
   * @param {object} opts.env
   * @param {number} [opts.requestTimeoutMs]
   */
  constructor(opts) {
    super()
    this.command = opts.command
    this.args = opts.args ?? []
    this.cwd = opts.cwd
    this.env = opts.env ?? process.env
    this.requestTimeoutMs = opts.requestTimeoutMs ?? 30000

    this.child = null
    this.rl = null
    this.pending = new Map()
    this.nextId = 1
    this.exited = false
    this.exitInfo = null

    /** 由 PluginHost 注入：处理插件 → 宿主的请求 */
    this.onRequest = null
  }

  start() {
    this.child = spawn(this.command, this.args, {
      cwd: this.cwd,
      env: this.env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true
    })

    // stderr 不当错误看：插件把日志写这里，转发出去做 `plugin.log` 事件。
    this.child.stderr.setEncoding('utf8')
    this.child.stderr.on('data', (chunk) => {
      for (const line of chunk.split(/\r?\n/)) {
        if (line.trim() !== '') this.emit('stderr', line)
      }
    })

    this.rl = readline.createInterface({ input: this.child.stdout, crlfDelay: Infinity })
    this.rl.on('line', (line) => this._handleLine(line))

    this.child.on('error', (err) => {
      this.emit('failure', new PluginError(ErrorCodes.PLUGIN_CRASHED, `进程启动失败：${err.message}`))
    })

    this.child.on('exit', (code, signal) => {
      this.exited = true
      this.exitInfo = { code, signal }
      // 所有在途请求立即失败，避免调用方永久等待。
      for (const [, entry] of this.pending) {
        clearTimeout(entry.timer)
        entry.reject(
          new PluginError(ErrorCodes.PLUGIN_CRASHED, `插件进程退出（code=${code}, signal=${signal}）`)
        )
      }
      this.pending.clear()
      this.emit('exit', { code, signal })
    })

    return this
  }

  _handleLine(line) {
    if (line.trim() === '') return
    if (Buffer.byteLength(line, 'utf8') > MAX_FRAME_BYTES) {
      this.emit('violation', new PluginError(ErrorCodes.PLUGIN_PROTOCOL_VIOLATION, '单帧超过 1 MiB'))
      this.kill()
      return
    }

    let frame
    try {
      frame = parseFrame(line)
    } catch (err) {
      this.emit('violation', err)
      return
    }

    const isResponse = frame.method === undefined && frame.id !== undefined
    if (isResponse) {
      const entry = this.pending.get(frame.id)
      if (!entry) {
        this.emit('stderr', `[supervisor] 收到未知响应 id=${frame.id}`)
        return
      }
      this.pending.delete(frame.id)
      clearTimeout(entry.timer)
      if (frame.error) {
        entry.reject(
          new PluginError(frame.error.code ?? ErrorCodes.INTERNAL_ERROR, frame.error.message, frame.error.data)
        )
      } else {
        entry.resolve(frame.result)
      }
      return
    }

    // 插件 → 宿主的请求 / 通知
    const isNotification = frame.id === undefined
    if (typeof frame.method !== 'string') {
      this.emit('violation', new PluginError(ErrorCodes.PLUGIN_PROTOCOL_VIOLATION, '帧缺少 method'))
      return
    }
    if (!this.onRequest) {
      this.emit('violation', new PluginError(ErrorCodes.PLUGIN_PROTOCOL_VIOLATION, '宿主未准备就绪'))
      return
    }

    Promise.resolve()
      .then(() => this.onRequest(frame.method, frame.params ?? {}))
      .then((result) => {
        if (!isNotification) {
          this._send({ jsonrpc: '2.0', id: frame.id, result: result ?? null })
        }
      })
      .catch((err) => {
        const error =
          err instanceof PluginError
            ? err.toObject()
            : { code: ErrorCodes.INTERNAL_ERROR, message: String(err?.message ?? err) }
        if (!isNotification) {
          this._send({ jsonrpc: '2.0', id: frame.id, error })
        } else {
          this.emit('stderr', `[plugin notify ${frame.method}] ${error.message}`)
        }
      })
  }

  _send(frame) {
    if (!this.child || this.exited || !this.child.stdin.writable) return false
    this.child.stdin.write(encodeFrame(frame))
    return true
  }

  request(method, params, timeoutMs = this.requestTimeoutMs) {
    if (this.exited) {
      return Promise.reject(new PluginError(ErrorCodes.PLUGIN_CRASHED, '插件进程未运行'))
    }
    const id = `h-${this.nextId++}`
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(
          new PluginError(ErrorCodes.PLUGIN_TIMEOUT, `${method} 超时 ${timeoutMs}ms`, { method, timeoutMs })
        )
      }, timeoutMs)
      // timer 不阻塞进程退出
      if (typeof timer.unref === 'function') timer.unref()

      this.pending.set(id, { resolve, reject, timer })
      const ok = this._send(requestFrame(id, method, params))
      if (!ok) {
        clearTimeout(timer)
        this.pending.delete(id)
        reject(new PluginError(ErrorCodes.PLUGIN_CRASHED, '管道不可写'))
      }
    })
  }

  notify(method, params) {
    const frame = { jsonrpc: '2.0', method }
    if (params !== undefined) frame.params = params
    return this._send(frame)
  }

  kill() {
    if (this.child && !this.exited) {
      try {
        this.child.kill('SIGKILL')
      } catch {
        /* 已退出则忽略 */
      }
    }
  }

  /** 优雅停机：先 shutdown 请求，超时再 kill。 */
  async stop({ timeoutMs = 3000 } = {}) {
    if (this.exited) return
    try {
      await this.request('plugin/shutdown', {}, timeoutMs)
    } catch {
      /* 插件不配合也要收尸 */
    }
    if (this.rl) this.rl.close()
    if (this.child && !this.exited) {
      try {
        this.child.stdin.end()
      } catch {
        /* ignore */
      }
      await new Promise((resolve) => {
        const timer = setTimeout(() => {
          this.kill()
          resolve()
        }, timeoutMs)
        this.child.once('exit', () => {
          clearTimeout(timer)
          resolve()
        })
      })
    }
  }
}
