/**
 * 密钥的「拦截—托管—重放」三件事。
 *
 * 为什么需要重放：内核出于安全只在**内存**里保管密钥（`providers.json` 只记 `key_ref`），
 * 所以内核每次重启后都是「有 Provider 配置但没有密钥」的状态。托管在 `credentials.enc`
 * 里的密钥必须由主进程在内核就绪后重新注入一次，否则用户在设置界面填过的 Key 会
 * 表现为「填了但连不上」。
 */

import {
  Methods,
  type ProviderListResult,
  type ProviderUpsertParams,
  type ProviderUpsertResult,
  type ProviderView
} from '@ai-ide/shared-types'

import type { CredentialVault } from './credentials'

function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * 从 `provider.upsert` 的参数里取出待托管的明文密钥。
 *
 * 返回 `null` 表示「这次调用与密钥无关」（不带 `api_key`，或根本不是 upsert）。
 * `providerId` 为 `null` 时说明是新建，真实 id 要等内核回执。
 */
export function extractUpsertedKey(
  method: string,
  params: unknown
): { providerId: string | null; apiKey: string } | null {
  if (method !== Methods.provider.upsert) return null
  if (!params || typeof params !== 'object') return null

  const record = params as Record<string, unknown>
  const apiKey = typeof record.api_key === 'string' ? record.api_key : ''
  if (apiKey.trim().length === 0) return null

  const rawId = record.id
  const providerId = typeof rawId === 'string' && rawId.trim().length > 0 ? rawId : null
  return { providerId, apiKey }
}

/** 从 `provider.delete` 的参数里取出 provider id；非该方法的调用返回 `null`。 */
export function extractDeletedProviderId(method: string, params: unknown): string | null {
  if (method !== Methods.provider.delete) return null
  if (!params || typeof params !== 'object') return null
  const record = params as Record<string, unknown>
  const providerId = record.provider_id
  return typeof providerId === 'string' && providerId.trim().length > 0 ? providerId : null
}

/**
 * `provider.upsert` 成功之后托管密钥。
 *
 * 返回落盘是否成功；`false` 只表示「只在内存」（safeStorage 不可用），
 * 不应当被当作失败——内核那边确实已经拿到了密钥。
 */
export async function rememberKeyAfterUpsert(
  vault: CredentialVault,
  result: unknown,
  pending: { providerId: string | null; apiKey: string }
): Promise<{ providerId: string; persisted: boolean }> {
  const providerId =
    pending.providerId ?? (result as ProviderUpsertResult | undefined)?.provider_id ?? null
  if (!providerId) {
    throw new Error('内核未回执 provider_id，无法托管密钥')
  }
  const persisted = await vault.save(providerId, pending.apiKey)
  return { providerId, persisted }
}

export interface ReplayReport {
  /** 成功注回内核的 provider id。 */
  injected: string[]
  /** 内核认为「有密钥」但本地托管缺失——只能由用户重新填写。 */
  missing: string[]
  failed: Array<{ providerId: string; message: string }>
}

export interface ReplayDeps {
  request: (method: string, params?: unknown) => Promise<unknown>
  vault: CredentialVault
  log: (text: string) => void
}

/**
 * 内核就绪后重放已托管密钥。
 *
 * 只重放「本地托管里有」的 Provider：托管缺失但内核声称有密钥的情况无法凭空恢复，
 * 记进 `missing` 让调用方提示用户重填，而不是假装成功。
 */
export async function replayCredentials(deps: ReplayDeps): Promise<ReplayReport> {
  const report: ReplayReport = { injected: [], missing: [], failed: [] }

  let providers: ProviderView[]
  try {
    const listed = (await deps.request(Methods.provider.list, {})) as ProviderListResult
    providers = Array.isArray(listed?.providers) ? listed.providers : []
  } catch (error) {
    deps.log(`[credentials] 读取 Provider 清单失败，跳过密钥重放：${reason(error)}\n`)
    return report
  }

  for (const provider of providers) {
    const apiKey = deps.vault.get(provider.id)
    if (!apiKey) {
      if (provider.has_key) {
        report.missing.push(provider.id)
        deps.log(`[credentials] ${provider.name}：内核侧留有密钥引用但本地托管缺失，需重新填写\n`)
      }
      continue
    }

    const params: ProviderUpsertParams = {
      id: provider.id,
      name: provider.name,
      protocol: provider.protocol,
      base_url: provider.base_url,
      api_key: apiKey,
      headers: provider.headers,
      enabled: provider.enabled,
      timeout_ms: provider.timeout_ms
    }

    try {
      await deps.request(Methods.provider.upsert, params)
      report.injected.push(provider.id)
    } catch (error) {
      report.failed.push({ providerId: provider.id, message: reason(error) })
    }
  }

  return report
}
