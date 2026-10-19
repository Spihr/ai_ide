/**
 * Provider 设置面板（M2 外壳侧）。
 *
 * 三段：托管状态、Provider 列表与表单、模型清单。
 * 密钥输入框**只写不读**——保存后界面上再也拿不回明文，只能看到内核回的 `key_hint`
 * （如 `sk-****abcd`），这与 docs/02 §6.3 的密钥流一致。
 */

import { useState } from 'react'

import type { ProviderProtocol, ProviderView } from '@ai-ide/shared-types'

import {
  addModel,
  fetchUpstreamModels,
  removeModel,
  removeProvider,
  saveProvider,
  selectProvider,
  testProvider,
  useAi
} from '../ai-store'

const PROTOCOLS: Array<{ value: ProviderProtocol; label: string }> = [
  { value: 'openai_chat_completions', label: 'OpenAI 兼容（chat/completions）' },
  { value: 'openai_responses', label: 'OpenAI Responses' },
  { value: 'anthropic_messages', label: 'Anthropic Messages' },
  { value: 'gemini', label: 'Gemini' },
  { value: 'ollama', label: 'Ollama（本地）' }
]

interface FormState {
  id: string | null
  name: string
  protocol: ProviderProtocol
  base_url: string
  api_key: string
  enabled: boolean
  timeout_ms: string
}

const EMPTY_FORM: FormState = {
  id: null,
  name: '',
  protocol: 'openai_chat_completions',
  base_url: 'https://api.openai.com/v1',
  api_key: '',
  enabled: true,
  timeout_ms: ''
}

function toForm(provider: ProviderView): FormState {
  return {
    id: provider.id,
    name: provider.name,
    protocol: provider.protocol,
    base_url: provider.base_url,
    api_key: '',
    enabled: provider.enabled,
    timeout_ms: provider.timeout_ms === null ? '' : String(provider.timeout_ms)
  }
}

interface ProviderFormProps {
  provider: ProviderView | null
}

/**
 * Provider 编辑表单。
 *
 * 选中项由父组件用 `key` 控制重挂载：切换 Provider 时组件整体重建，
 * 表单初始值直接来自新的 `provider`，因此**不需要** effect 去同步外部状态，
 * 也就不会产生 effect 内 setState 引发的级联渲染。
 * `api_key` 始终以空串起步——不回填明文。
 */
function ProviderForm({ provider }: ProviderFormProps): React.JSX.Element {
  const state = useAi()
  const [form, setForm] = useState<FormState>(() => (provider ? toForm(provider) : EMPTY_FORM))

  const onSave = async (): Promise<void> => {
    const timeout = form.timeout_ms.trim()
    const saved = await saveProvider({
      id: form.id,
      name: form.name,
      protocol: form.protocol,
      base_url: form.base_url,
      api_key: form.api_key,
      enabled: form.enabled,
      timeout_ms: timeout.length > 0 ? Number(timeout) : null
    })
    if (saved) setForm((previous) => ({ ...previous, api_key: '' }))
  }

  return (
    <>
      <label className="ai-field">
        <span>名称</span>
        <input
          className="ai-input"
          data-testid="provider-name"
          value={form.name}
          onChange={(event) => setForm({ ...form, name: event.target.value })}
          placeholder="如 本地假上游"
        />
      </label>

      <label className="ai-field">
        <span>协议</span>
        <select
          className="ai-input"
          data-testid="provider-protocol"
          value={form.protocol}
          onChange={(event) => setForm({ ...form, protocol: event.target.value as ProviderProtocol })}
        >
          {PROTOCOLS.map((item) => (
            <option key={item.value} value={item.value}>
              {item.label}
            </option>
          ))}
        </select>
      </label>

      <label className="ai-field">
        <span>Base URL</span>
        <input
          className="ai-input"
          data-testid="provider-base-url"
          value={form.base_url}
          onChange={(event) => setForm({ ...form, base_url: event.target.value })}
          placeholder="https://api.openai.com/v1"
        />
      </label>

      <label className="ai-field">
        <span>API Key{form.id ? '（留空表示不改）' : ''}</span>
        <input
          className="ai-input"
          data-testid="provider-api-key"
          type="password"
          autoComplete="off"
          value={form.api_key}
          onChange={(event) => setForm({ ...form, api_key: event.target.value })}
          placeholder={provider?.has_key ? (provider.key_hint ?? '已存密钥') : 'sk-...'}
        />
      </label>

      <div className="ai-row">
        <label className="ai-check">
          <input
            type="checkbox"
            data-testid="provider-enabled"
            checked={form.enabled}
            onChange={(event) => setForm({ ...form, enabled: event.target.checked })}
          />
          启用
        </label>
        <label className="ai-check">
          超时(ms)
          <input
            className="ai-input ai-input--narrow"
            data-testid="provider-timeout"
            value={form.timeout_ms}
            onChange={(event) => setForm({ ...form, timeout_ms: event.target.value })}
            placeholder="默认"
          />
        </label>
      </div>

      <div className="ai-row">
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="provider-save"
          disabled={state.loading}
          onClick={() => void onSave()}
        >
          保存
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="provider-test"
          disabled={state.test.running}
          onClick={() => void testProvider(form.id)}
        >
          {state.test.running ? '测试中…' : '测试连接'}
        </button>
        <button
          className="ai-button ai-button--danger"
          type="button"
          data-testid="provider-delete"
          disabled={form.id === null}
          onClick={() => {
            if (form.id) void removeProvider(form.id)
          }}
        >
          删除
        </button>
      </div>

      {state.test.text !== null ? (
        <div
          className={state.test.ok ? 'ai-result ai-result--ok' : 'ai-result ai-result--bad'}
          data-testid="provider-test-result"
        >
          {state.test.text}
        </div>
      ) : null}

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="provider-error">
          {state.error}
        </div>
      ) : null}
    </>
  )
}

export function ProviderSettings(): React.JSX.Element {
  const state = useAi()
  const [modelId, setModelId] = useState('')

  const activeProvider = state.providers.find((item) => item.id === state.activeProviderId) ?? null
  const managed = state.credentials?.managedProviderIds ?? []

  return (
    <div className="ai-section" data-testid="provider-settings">
      <h3 className="ai-section__title">Provider 设置</h3>

      <div className="ai-note" data-testid="credential-status">
        {state.credentials === null
          ? '密钥托管状态未知'
          : state.credentials.encryptionAvailable
            ? `密钥经系统密钥链加密托管，已托管 ${managed.length} 个 Provider`
            : '系统密钥链不可用：密钥只留在内存中，关闭应用后需重新填写'}
        {state.credentials?.lastError ? (
          <div className="ai-note__warn">{state.credentials.lastError}</div>
        ) : null}
      </div>

      <div className="ai-row">
        <select
          className="ai-input"
          data-testid="provider-select"
          value={state.activeProviderId ?? ''}
          onChange={(event) => selectProvider(event.target.value === '' ? null : event.target.value)}
        >
          <option value="">（新建 Provider）</option>
          {state.providers.map((provider) => (
            <option key={provider.id} value={provider.id}>
              {provider.name}
              {provider.has_key ? ` · ${provider.key_hint ?? '已存密钥'}` : ' · 无密钥'}
            </option>
          ))}
        </select>
        <button className="ai-button" type="button" onClick={() => selectProvider(null)}>
          新建
        </button>
      </div>

      <ProviderForm key={activeProvider?.id ?? 'new-provider'} provider={activeProvider} />

      <h4 className="ai-section__subtitle">模型</h4>
      {state.activeProviderId === null ? (
        <div className="ai-note">先保存或选中一个 Provider，再来登记模型。</div>
      ) : (
        <>
          <div className="ai-row">
            <input
              className="ai-input"
              data-testid="model-id"
              value={modelId}
              onChange={(event) => setModelId(event.target.value)}
              placeholder="上游模型 id，如 gpt-4o-mini"
            />
            <button
              className="ai-button"
              type="button"
              data-testid="model-add"
              disabled={modelId.trim().length === 0}
              onClick={() => {
                if (state.activeProviderId) {
                  void addModel(state.activeProviderId, modelId, '')
                  setModelId('')
                }
              }}
            >
              登记
            </button>
            <button
              className="ai-button"
              type="button"
              data-testid="model-fetch-upstream"
              onClick={() => {
                if (state.activeProviderId) void fetchUpstreamModels(state.activeProviderId)
              }}
            >
              拉取上游清单
            </button>
          </div>

          {state.upstreamModels.length > 0 ? (
            <div className="ai-note" data-testid="upstream-models">
              上游可用 {state.upstreamModels.length} 个：
              {state.upstreamModels.slice(0, 8).map((model) => model.id).join('、')}
              {state.upstreamModels.length > 8 ? ' …' : ''}
            </div>
          ) : null}

          <ul className="ai-list" data-testid="model-list">
            {state.models.map((model) => (
              <li key={model.model_pk} className="ai-list__item">
                <span>{model.display_name ?? model.model_id}</span>
                <span className="ai-list__meta">{model.model_id}</span>
                <button
                  className="ai-button ai-button--tiny"
                  type="button"
                  onClick={() => void removeModel(model.model_pk)}
                >
                  移除
                </button>
              </li>
            ))}
            {state.models.length === 0 ? <li className="ai-note">还没有登记模型</li> : null}
          </ul>
        </>
      )}
    </div>
  )
}
