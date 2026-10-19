/**
 * 主进程侧的密钥托管（Credential Vault）。
 *
 * 架构约定（docs/02 §6.3）：Provider 的 API Key 只有两条存活路径——
 *   1. 渲染进程 → 主进程内存 → `safeStorage` 加密后写入 `<userData>/credentials.enc`；
 *   2. 主进程解密 → JSON-RPC 交给内核，内核只放在内存里（`providers.json` 只记 `key_ref`）。
 *
 * 两条硬约束：
 * - **绝不落明文**：`safeStorage` 不可用（缺系统密钥链）时只留在内存，宁可放弃持久化，
 *   也不退化成明文写盘；
 * - **绝不给渲染进程明文**：`status()` 只回「有没有托管」与文件位置，不回密钥本身。
 */

import { mkdir, readFile, rename, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'

import type { CredentialStatus } from '../shared/ipc'

/** `safeStorage` 里我们真正用到的那部分（便于单测注入假实现）。 */
export interface SafeStorageLike {
  isEncryptionAvailable(): boolean
  encryptString(plainText: string): Buffer
  decryptString(encrypted: Buffer): string
}

const FILE_VERSION = 1

interface VaultFile {
  version: number
  /** provider id → 加密后密钥的 base64。 */
  keys: Record<string, string>
}

export interface CredentialVaultOptions {
  /** 托管文件绝对路径（`<userData>/credentials.enc`）。 */
  vaultPath: string
  safeStorage: SafeStorageLike
}

function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

export class CredentialVault {
  private readonly vaultPath: string
  private readonly safeStorage: SafeStorageLike
  /** 内存镜像：加密不可用时的唯一存活位置，也是未落盘时的真相来源。 */
  private readonly keys = new Map<string, string>()
  private lastError: string | null = null
  /** 写盘串行化：两次保存并发时不能交错读改写。 */
  private writeQueue: Promise<void> = Promise.resolve()

  constructor(options: CredentialVaultOptions) {
    this.vaultPath = options.vaultPath
    this.safeStorage = options.safeStorage
  }

  get path(): string {
    return this.vaultPath
  }

  /** 读取已有托管文件。文件损坏时保留原文件并记下原因，不静默清空。 */
  async load(): Promise<void> {
    let text: string
    try {
      text = await readFile(this.vaultPath, 'utf8')
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return
      this.lastError = `读取密钥托管文件失败：${reason(error)}`
      return
    }

    let parsed: VaultFile
    try {
      parsed = JSON.parse(text) as VaultFile
    } catch {
      this.lastError = '密钥托管文件损坏（不是合法 JSON），已忽略其内容但未删除该文件'
      return
    }

    if (!parsed || typeof parsed !== 'object' || typeof parsed.keys !== 'object' || parsed.keys === null) {
      this.lastError = '密钥托管文件结构无法识别，已忽略其内容但未删除该文件'
      return
    }

    if (!this.safeStorage.isEncryptionAvailable()) {
      this.lastError = '托管文件存在但系统密钥链不可用，无法解密；请重新填写密钥'
      return
    }

    for (const [providerId, encoded] of Object.entries(parsed.keys)) {
      if (typeof encoded !== 'string') continue
      try {
        this.keys.set(providerId, this.safeStorage.decryptString(Buffer.from(encoded, 'base64')))
      } catch (error) {
        this.lastError = `密钥 ${providerId} 解密失败（换了系统账号或文件被篡改）：${reason(error)}`
      }
    }
  }

  status(): CredentialStatus {
    return {
      encryptionAvailable: this.safeStorage.isEncryptionAvailable(),
      vaultPath: this.vaultPath,
      managedProviderIds: [...this.keys.keys()].sort(),
      lastError: this.lastError
    }
  }

  has(providerId: string): boolean {
    return this.keys.has(providerId)
  }

  /** 取明文密钥：只允许主进程内部使用（注入内核），不得经 IPC 外发。 */
  get(providerId: string): string | null {
    return this.keys.get(providerId) ?? null
  }

  managedProviderIds(): string[] {
    return [...this.keys.keys()].sort()
  }

  /** 托管一把密钥。返回是否**落盘成功**；`false` 表示只在内存里。 */
  async save(providerId: string, apiKey: string): Promise<boolean> {
    const trimmed = apiKey.trim()
    if (trimmed.length === 0) throw new Error('密钥不能为空')

    this.keys.set(providerId, trimmed)

    if (!this.safeStorage.isEncryptionAvailable()) {
      this.lastError = '系统密钥链不可用（safeStorage 未就绪），密钥仅保留在内存中，未写入磁盘'
      return false
    }

    try {
      await this.persist()
      this.lastError = null
      return true
    } catch (error) {
      this.lastError = `写入密钥托管文件失败：${reason(error)}`
      return false
    }
  }

  /** 撤销托管（删除 Provider 时调用）。返回此前是否托管过。 */
  async forget(providerId: string): Promise<boolean> {
    const existed = this.keys.delete(providerId)
    if (!existed) return false
    if (this.safeStorage.isEncryptionAvailable()) {
      try {
        await this.persist()
        this.lastError = null
      } catch (error) {
        this.lastError = `写入密钥托管文件失败：${reason(error)}`
      }
    }
    return true
  }

  private persist(): Promise<void> {
    this.writeQueue = this.writeQueue.then(() => this.writeNow())
    return this.writeQueue
  }

  /** 先写临时文件再改名：中途失败不会留下半截 JSON 顶掉可用的旧文件。 */
  private async writeNow(): Promise<void> {
    const payload: VaultFile = { version: FILE_VERSION, keys: {} }
    for (const [providerId, plain] of this.keys) {
      payload.keys[providerId] = this.safeStorage.encryptString(plain).toString('base64')
    }

    await mkdir(dirname(this.vaultPath), { recursive: true })
    const tempPath = `${this.vaultPath}.tmp`
    await writeFile(tempPath, JSON.stringify(payload, null, 2), { encoding: 'utf8', mode: 0o600 })
    await rename(tempPath, this.vaultPath)
  }
}
