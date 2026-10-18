/**
 * 内核进程的监护人。
 *
 * 职责：
 * 1. 以子进程方式拉起 `ai-ide-core`，接管 stdin/stdout 管道；
 * 2. 把 stdout 的 NDJSON 流解析成 JSON-RPC 帧，按 `id` 与在途请求配对；
 * 3. 提供 `request` / `notify` 两个抽象，把「进程通信」这件事对上层完全隐藏；
 * 4. 进程意外退出时，把在途请求全部失败掉，并发出 `exit` 事件。
 *
 * 注意：这一层**不做**任何策略判断。是否允许某个操作，由 `policy.ts` 决定。
 */

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { randomUUID } from 'node:crypto'
import { createInterface, type Interface } from 'node:readline'
import { existsSync } from 'node:fs'

import {
  JSONRPC_VERSION,
  Methods,
  type ErrorObject,
  type HelloResult,
  type Notification as RpcNotification,
  type Response as RpcResponse,
  type ErrorResponse as RpcErrorResponse
} from '@ai-ide/shared-types'

/** 内核返回的错误，包装成 JS 异常抛出。 */
export class CoreRpcError extends Error {
  readonly code: number
  readonly data: unknown
  readonly method: string

  constructor(error: ErrorObject, method: string) {
    super(`[${error.code}] ${error.message}`)
    this.name = 'CoreRpcError'
    this.code = error.code
    this.data = error.data
    this.method = method
  }
}

export interface CoreSupervisorOptions {
  /** 内核可执行文件的绝对路径。 */
  binaryPath: string
  /** 内核工作目录，默认继承当前进程。 */
  cwd?: string
  /** 客户端版本号，用于握手。 */
  clientVersion?: string
  /** 单条请求超时（毫秒），默认 30s。 */
  requestTimeoutMs?: number
}

interface PendingRequest {
  resolve: (value: unknown) => void
  reject: (reason: Error) => void
  timer: NodeJS.Timeout
  method: string
}

/** 事件签名。 */
export interface CoreSupervisorEvents {
  /** 内核 stderr 输出（内核日志）。 */
  stderr: [chunk: string]
  /** 内核主动推送的通知。 */
  notification: [method: string, params: unknown]
  /** 内核进程退出。 */
  exit: [code: number | null, signal: NodeJS.Signals | null]
  /** 收到无法识别的报文。 */
  protocolError: [detail: string]
}

export class CoreSupervisor extends EventEmitter<CoreSupervisorEvents> {
  private child: ChildProcessWithoutNullStreams | null = null
  private reader: Interface | null = null
  private readonly pending = new Map<string, PendingRequest>()

  private readonly binaryPath: string
  private readonly cwd: string | undefined
  private readonly clientVersion: string
  private readonly requestTimeoutMs: number

  /** 握手结果，进程就绪后可用。 */
  private hello: HelloResult | null = null
  /** 最近一次异常退出信息。 */
  private lastExit: { code: number | null; signal: NodeJS.Signals | null } | null = null

  constructor(options: CoreSupervisorOptions) {
    super()
    this.binaryPath = options.binaryPath
    this.cwd = options.cwd
    this.clientVersion = options.clientVersion ?? '0.1.0'
    this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000
  }

  /** 内核是否处于可用状态。 */
  get isRunning(): boolean {
    return this.child !== null && this.child.exitCode === null && !this.child.killed
  }

  /** 握手结果（未完成握手时为 `null`）。 */
  get helloResult(): HelloResult | null {
    return this.hello
  }

  /** 上次退出信息（正常运行时为 `null`）。 */
  get exitInfo(): { code: number | null; signal: NodeJS.Signals | null } | null {
    return this.lastExit
  }

  /** 内核 pid，未运行时为 `null`。 */
  get pid(): number | null {
    return this.child?.pid ?? null
  }

  /**
   * 拉起内核并完成握手。
   *
   * @throws 内核文件缺失、进程启动失败、握手失败或协议版本不兼容时抛出。
   */
  async start(): Promise<HelloResult> {
    if (this.isRunning) {
      throw new Error('内核已在运行，重复 start 被拒绝')
    }
    if (!existsSync(this.binaryPath)) {
      throw new Error(
        `找不到内核可执行文件：${this.binaryPath}\n` +
          '请先执行 `pnpm core:build`（或 `cargo build -p ai-ide-core`）'
      )
    }

    const child = spawn(this.binaryPath, [], {
      cwd: this.cwd,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true
    })
    this.child = child
    this.lastExit = null

    this.reader = createInterface({ input: child.stdout, crlfDelay: Infinity })
    this.reader.on('line', (line) => this.handleLine(line))

    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (chunk: string) => this.emit('stderr', chunk))

    child.on('error', (err) => {
      this.failAllPending(new Error(`内核进程错误：${err.message}`))
    })

    child.on('exit', (code, signal) => {
      this.lastExit = { code, signal }
      this.reader?.close()
      this.reader = null
      this.child = null
      this.hello = null
      this.failAllPending(
        new Error(`内核进程已退出（code=${code ?? 'null'}, signal=${signal ?? 'null'}）`)
      )
      this.emit('exit', code, signal)
    })

    const hello = await this.request<HelloResult>(Methods.core.hello, {
      client_version: this.clientVersion,
      protocol_version: { major: 1, minor: 0 },
      platform: process.platform,
      locale: 'zh-CN',
      capabilities: []
    })

    this.hello = hello
    return hello
  }

  /**
   * 发起一次请求并等待结果。
   *
   * 注意：不传 `params` 时补 `{}`，因为内核侧的空参数 DTO 只接受对象。
   */
  request<T>(method: string, params?: unknown): Promise<T> {
    const child = this.child
    if (!child || child.exitCode !== null || child.killed) {
      return Promise.reject(new Error(`内核未运行，无法调用 ${method}`))
    }

    const id = randomUUID()
    const line =
      JSON.stringify({
        jsonrpc: JSONRPC_VERSION,
        id,
        method,
        params: params ?? {}
      }) + '\n'

    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`请求超时（${this.requestTimeoutMs}ms）：${method}`))
      }, this.requestTimeoutMs)

      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        timer,
        method
      })

      child.stdin.write(line, (err) => {
        if (err) {
          this.settle(id, () => reject(new Error(`写入内核 stdin 失败：${err.message}`)))
        }
      })
    })
  }

  /** 发送一条通知（不等待结果）。 */
  notify(method: string, params?: unknown): void {
    const child = this.child
    if (!child || child.exitCode !== null || child.killed) {
      this.emit('protocolError', `内核未运行，通知被丢弃：${method}`)
      return
    }
    const line =
      JSON.stringify({
        jsonrpc: JSONRPC_VERSION,
        method,
        params: params ?? {}
      }) + '\n'
    child.stdin.write(line, (err) => {
      if (err) this.emit('protocolError', `写入内核 stdin 失败：${err.message}`)
    })
  }

  /** 优雅退出内核；超时未退则强杀。 */
  async stop(graceful = true, timeoutMs = 3_000): Promise<void> {
    const child = this.child
    if (!child) return

    if (graceful && child.exitCode === null) {
      try {
        await this.request(Methods.core.shutdown, { graceful: true })
      } catch {
        // 内核可能已经自己退了，忽略
      }
    }

    const exited = await waitForExit(child, timeoutMs)
    if (!exited) {
      child.kill('SIGKILL')
      await waitForExit(child, 1_000)
    }

    this.child = null
    this.hello = null
    this.failAllPending(new Error('内核已停止'))
  }

  // -------------------------------------------------------------------------
  // 内部
  // -------------------------------------------------------------------------

  private handleLine(line: string): void {
    const text = line.trim()
    if (text.length === 0) return

    let frame: unknown
    try {
      frame = JSON.parse(text)
    } catch (err) {
      this.emit('protocolError', `内核输出了非法 JSON：${(err as Error).message}`)
      return
    }

    if (typeof frame !== 'object' || frame === null) {
      this.emit('protocolError', `内核输出了非对象报文：${text.slice(0, 200)}`)
      return
    }

    const candidate = frame as Partial<RpcResponse & RpcErrorResponse & RpcNotification>

    // 带 id 且有 result / error —— 是响应
    if (
      typeof candidate.id === 'string' &&
      ('result' in candidate || 'error' in candidate)
    ) {
      this.handleResponse(candidate as RpcResponse & RpcErrorResponse)
      return
    }

    // 有 method 无 id —— 是通知
    if (typeof candidate.method === 'string') {
      this.emit('notification', candidate.method, candidate.params)
      return
    }

    this.emit('protocolError', `无法识别的报文：${text.slice(0, 200)}`)
  }

  private handleResponse(frame: (RpcResponse & RpcErrorResponse) | Partial<RpcResponse>): void {
    const id = frame.id
    if (typeof id !== 'string') return

    const pending = this.pending.get(id)
    if (!pending) {
      this.emit('protocolError', `收到未知 id 的响应：${id}`)
      return
    }
    this.pending.delete(id)
    clearTimeout(pending.timer)

    if ('error' in frame && frame.error) {
      pending.reject(new CoreRpcError(frame.error, pending.method))
      return
    }
    pending.resolve((frame as RpcResponse).result)
  }

  private settle(id: string, action: () => void): void {
    const pending = this.pending.get(id)
    if (!pending) return
    this.pending.delete(id)
    clearTimeout(pending.timer)
    action()
  }

  private failAllPending(reason: Error): void {
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer)
      this.pending.delete(id)
      pending.reject(reason)
    }
  }
}

/** 等待子进程退出，返回是否在超时前退出。 */
function waitForExit(
  child: ChildProcessWithoutNullStreams,
  timeoutMs: number
): Promise<boolean> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true)
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off('exit', onExit)
      resolve(false)
    }, timeoutMs)
    const onExit = (): void => {
      clearTimeout(timer)
      resolve(true)
    }
    child.once('exit', onExit)
  })
}
