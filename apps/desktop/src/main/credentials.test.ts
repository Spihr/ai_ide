/**
 * 密钥托管（`CredentialVault`）的单元测试。
 *
 * 这块的失败代价是「用户的 API Key 明文躺在磁盘上」，所以测试的重点不是功能有多全，
 * 而是三条硬约束：
 *   1. `safeStorage` 可用时，落盘内容里**不能**出现明文（这里用可逆的假实现，
 *      密文里能直接查到明文子串）；
 *   2. `safeStorage` 不可用时**宁可只留内存**，也不退化成明文写盘；
 *   3. `status()` 是给渲染进程看的，绝不能夹带密钥内容。
 */

import { mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { existsSync } from 'node:fs'

import { beforeEach, describe, expect, it } from 'vitest'

import { CredentialVault, type SafeStorageLike } from './credentials'

const SECRET = 'sk-plain-secret-0f2c'

/** 可逆的假 safeStorage：密文形如 `enc:sk-...`，于是「文件里有没有明文」一眼可查。 */
function fakeSafeStorage(available = true): SafeStorageLike {
  return {
    isEncryptionAvailable: () => available,
    encryptString: (plainText) => Buffer.from(`enc:${plainText}`, 'utf8'),
    decryptString: (encrypted) => {
      const text = encrypted.toString('utf8')
      if (!text.startsWith('enc:')) throw new Error('密文格式不对')
      return text.slice(4)
    }
  }
}

let dir: string
let vaultPath: string

beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), 'ai-ide-vault-'))
  vaultPath = join(dir, 'credentials.enc')
})

describe('CredentialVault', () => {
  it('托管后落盘的是密文，明文不出现在文件里', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()

    expect(await vault.save('p-1', SECRET)).toBe(true)

    const text = await readFile(vaultPath, 'utf8')
    expect(text).not.toContain(SECRET)
    expect(text).toContain('p-1')
  })

  it('重新加载后能解回同一把密钥', async () => {
    const first = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await first.load()
    await first.save('p-1', SECRET)

    const second = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await second.load()

    expect(second.has('p-1')).toBe(true)
    expect(second.get('p-1')).toBe(SECRET)
    expect(second.managedProviderIds()).toEqual(['p-1'])
  })

  it('系统密钥链不可用时只留内存，绝不写盘', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage(false) })
    await vault.load()

    expect(await vault.save('p-1', SECRET)).toBe(false)
    // 功能上仍然可用（内核要拿得到密钥），但没有任何文件落地。
    expect(vault.get('p-1')).toBe(SECRET)
    expect(existsSync(vaultPath)).toBe(false)
    expect(vault.status().lastError).toContain('仅保留在内存')
  })

  it('status 不含密钥内容，只回托管清单与文件位置', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()
    await vault.save('p-1', SECRET)

    const status = vault.status()
    expect(status.managedProviderIds).toEqual(['p-1'])
    expect(status.vaultPath).toBe(vaultPath)
    expect(status.encryptionAvailable).toBe(true)
    expect(JSON.stringify(status)).not.toContain(SECRET)
  })

  it('撤销托管会把密钥从内存与文件里一并抹掉', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()
    await vault.save('p-1', SECRET)

    expect(await vault.forget('p-1')).toBe(true)
    expect(vault.has('p-1')).toBe(false)
    expect(await vault.forget('p-1')).toBe(false)

    const reloaded = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await reloaded.load()
    expect(reloaded.managedProviderIds()).toEqual([])
  })

  it('空密钥拒绝托管', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()
    await expect(vault.save('p-1', '   ')).rejects.toThrow('密钥不能为空')
  })

  it('托管文件损坏时保留原文件并记下原因（不静默清空）', async () => {
    await writeFile(vaultPath, '这不是 JSON', 'utf8')

    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()

    expect(vault.managedProviderIds()).toEqual([])
    expect(vault.status().lastError).toContain('损坏')
    expect(await readFile(vaultPath, 'utf8')).toBe('这不是 JSON')
  })

  it('有托管文件但密钥链不可用时给出「需重新填写」的明确原因', async () => {
    const withKeys = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await withKeys.load()
    await withKeys.save('p-1', SECRET)

    const noChain = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage(false) })
    await noChain.load()

    expect(noChain.managedProviderIds()).toEqual([])
    expect(noChain.status().lastError).toContain('系统密钥链不可用')
  })

  it('连续保存不会互相覆盖（写盘串行化）', async () => {
    const vault = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await vault.load()

    await Promise.all([vault.save('p-1', 'sk-1'), vault.save('p-2', 'sk-2'), vault.save('p-3', 'sk-3')])

    const reloaded = new CredentialVault({ vaultPath, safeStorage: fakeSafeStorage() })
    await reloaded.load()
    expect(reloaded.managedProviderIds()).toEqual(['p-1', 'p-2', 'p-3'])
    expect(reloaded.get('p-2')).toBe('sk-2')
  })
})
