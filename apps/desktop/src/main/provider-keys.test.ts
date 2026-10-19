/**
 * 密钥「拦截—托管—重放」的单元测试。
 *
 * 这条链最容易出的错是「静默不生效」：拦截没拦住（明文只是过路，没托管）、
 * 新建 Provider 时 id 还没回执就丢了密钥、内核重启后忘了重放（表现为「填了 Key
 * 却连不上」）。所以这里逐段钉死行为，包括「托管缺失」这种必须如实上报的路径。
 */

import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { Methods, type ProviderView } from '@ai-ide/shared-types'

import { CredentialVault, type SafeStorageLike } from './credentials'
import {
  extractDeletedProviderId,
  extractUpsertedKey,
  rememberKeyAfterUpsert,
  replayCredentials,
  type ReplayReport
} from './provider-keys'

const SECRET = 'sk-replay-0f2c'

function fakeSafeStorage(): SafeStorageLike {
  return {
    isEncryptionAvailable: () => true,
    encryptString: (plainText) => Buffer.from(`enc:${plainText}`, 'utf8'),
    decryptString: (encrypted) => encrypted.toString('utf8').slice(4)
  }
}

function providerView(patch: Partial<ProviderView> = {}): ProviderView {
  return {
    id: 'p-1',
    name: '本地假上游',
    protocol: 'openai_chat_completions',
    base_url: 'http://127.0.0.1:8899/v1',
    headers: {},
    enabled: true,
    has_key: true,
    key_hint: 'sk-r…f2c',
    timeout_ms: null,
    last_test_ok: null,
    created_at_ms: 1,
    updated_at_ms: 1,
    ...patch
  }
}

let vault: CredentialVault

beforeEach(async () => {
  const dir = await mkdtemp(join(tmpdir(), 'ai-ide-keys-'))
  vault = new CredentialVault({ vaultPath: join(dir, 'credentials.enc'), safeStorage: fakeSafeStorage() })
  await vault.load()
})

describe('extractUpsertedKey', () => {
  it('取出带密钥的 upsert 参数', () => {
    expect(extractUpsertedKey(Methods.provider.upsert, { name: 'x', api_key: SECRET })).toEqual({
      providerId: null,
      apiKey: SECRET
    })
    expect(extractUpsertedKey(Methods.provider.upsert, { id: 'p-1', api_key: SECRET })).toEqual({
      providerId: 'p-1',
      apiKey: SECRET
    })
  })

  it('不带密钥 / 非 upsert / 参数畸形时返回 null（不误托管）', () => {
    expect(extractUpsertedKey(Methods.provider.upsert, { api_key: null })).toBeNull()
    expect(extractUpsertedKey(Methods.provider.upsert, { api_key: '   ' })).toBeNull()
    expect(extractUpsertedKey(Methods.provider.upsert, undefined)).toBeNull()
    expect(extractUpsertedKey(Methods.provider.list, { api_key: SECRET })).toBeNull()
    expect(extractUpsertedKey(Methods.provider.test, { api_key: SECRET })).toBeNull()
  })
})

describe('extractDeletedProviderId', () => {
  it('只认 provider.delete', () => {
    expect(extractDeletedProviderId(Methods.provider.delete, { provider_id: 'p-1' })).toBe('p-1')
    expect(extractDeletedProviderId(Methods.model.delete, { provider_id: 'p-1' })).toBeNull()
    expect(extractDeletedProviderId(Methods.provider.delete, { provider_id: '  ' })).toBeNull()
    expect(extractDeletedProviderId(Methods.provider.delete, undefined)).toBeNull()
  })
})

describe('rememberKeyAfterUpsert', () => {
  it('新建 Provider 时以内核回执的 provider_id 托管', async () => {
    const saved = await rememberKeyAfterUpsert(
      vault,
      { provider_id: 'p-new' },
      { providerId: null, apiKey: SECRET }
    )

    expect(saved).toEqual({ providerId: 'p-new', persisted: true })
    expect(vault.get('p-new')).toBe(SECRET)
  })

  it('内核没回执 provider_id 时报错（宁可报错也不要静默丢掉密钥）', async () => {
    await expect(
      rememberKeyAfterUpsert(vault, {}, { providerId: null, apiKey: SECRET })
    ).rejects.toThrow('无法托管密钥')
  })
})

describe('replayCredentials', () => {
  it('把托管密钥以 provider.upsert 注回内核', async () => {
    await vault.save('p-1', SECRET)
    const request = vi.fn(async (method: string, _params?: unknown) => {
      if (method === Methods.provider.list) return { providers: [providerView()] }
      return { provider_id: 'p-1' }
    })

    const report: ReplayReport = await replayCredentials({ request, vault, log: () => undefined })

    expect(report.injected).toEqual(['p-1'])
    expect(report.failed).toEqual([])

    const upsert = request.mock.calls.find(([method]) => method === Methods.provider.upsert)
    expect(upsert?.[1]).toMatchObject({
      id: 'p-1',
      name: '本地假上游',
      base_url: 'http://127.0.0.1:8899/v1',
      api_key: SECRET
    })
  })

  it('内核说「有密钥」但本地托管缺失时如实上报 missing，不假装成功', async () => {
    const request = vi.fn(async () => ({ providers: [providerView({ id: 'p-ghost', has_key: true })] }))
    const lines: string[] = []

    const report = await replayCredentials({ request, vault, log: (text) => lines.push(text) })

    expect(report.injected).toEqual([])
    expect(report.missing).toEqual(['p-ghost'])
    expect(lines.join('')).toContain('需重新填写')
  })

  it('内核侧已经没密钥的 Provider 不重放也不报缺失', async () => {
    await vault.save('p-1', SECRET)
    const request = vi.fn(async () => ({ providers: [providerView({ has_key: false })] }))

    const report = await replayCredentials({ request, vault, log: () => undefined })

    // 本地有密钥就注回去：用户填过的 Key 不该因为重启内核就表现成「没填」。
    expect(report.injected).toEqual(['p-1'])
    expect(report.missing).toEqual([])
  })

  it('单个 Provider 注入失败不影响其他 Provider', async () => {
    await vault.save('p-1', SECRET)
    await vault.save('p-2', SECRET)
    const request = vi.fn(async (method: string, params?: unknown) => {
      if (method === Methods.provider.list) {
        return { providers: [providerView({ id: 'p-1' }), providerView({ id: 'p-2' })] }
      }
      if ((params as { id?: string }).id === 'p-1') throw new Error('内核拒绝')
      return { provider_id: 'p-2' }
    })

    const report = await replayCredentials({ request, vault, log: () => undefined })

    expect(report.injected).toEqual(['p-2'])
    expect(report.failed).toHaveLength(1)
    expect(report.failed[0]?.providerId).toBe('p-1')
  })

  it('拉不到 Provider 清单时跳过重放并记日志，不抛错打断内核启动', async () => {
    const lines: string[] = []
    const request = vi.fn(async () => {
      throw new Error('内核未就绪')
    })

    const report = await replayCredentials({ request, vault, log: (text) => lines.push(text) })

    expect(report.injected).toEqual([])
    expect(lines.join('')).toContain('跳过密钥重放')
  })
})
