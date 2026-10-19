/**
 * M2 渲染侧 API 封装：把 `window.aiide.request` 收窄成业务动作。
 *
 * 与 `workspace-api.ts` 同一套路——组件不直接拼方法名与参数，避免
 * 「方法名拼错但到运行时才发现」。所有调用都会被主进程的策略闸门检查一次。
 */

import {
  Methods,
  type LlmCancelParams,
  type LlmChatStreamParams,
  type LlmChatStreamResult,
  type LlmCountTokensParams,
  type LlmCountTokensResult,
  type LlmListModelsParams,
  type LlmListModelsResult,
  type ModelDeleteParams,
  type ModelListParams,
  type ModelListResult,
  type ModelUpsertParams,
  type ModelUpsertResult,
  type OkResult,
  type ProviderDeleteParams,
  type ProviderListParams,
  type ProviderListResult,
  type ProviderTestParams,
  type ProviderTestResult,
  type ProviderUpsertParams,
  type ProviderUpsertResult
} from '@ai-ide/shared-types'

function request<T>(method: string, params?: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

export const aiApi = {
  listProviders: (): Promise<ProviderListResult> =>
    request<ProviderListResult>(Methods.provider.list, {} satisfies ProviderListParams),

  upsertProvider: (params: ProviderUpsertParams): Promise<ProviderUpsertResult> =>
    request<ProviderUpsertResult>(Methods.provider.upsert, params),

  deleteProvider: (providerId: string): Promise<OkResult> =>
    request<OkResult>(Methods.provider.delete, { provider_id: providerId } satisfies ProviderDeleteParams),

  /** 连接测试：`provider_id` 为空且带 `override` 时可测「还没保存的配置」。 */
  testProvider: (providerId: string | null, override?: ProviderTestParams['override']): Promise<ProviderTestResult> =>
    request<ProviderTestResult>(Methods.provider.test, {
      provider_id: providerId,
      override: override ?? null
    } satisfies ProviderTestParams),

  listModels: (providerId: string): Promise<ModelListResult> =>
    request<ModelListResult>(Methods.model.list, { provider_id: providerId } satisfies ModelListParams),

  upsertModel: (params: ModelUpsertParams): Promise<ModelUpsertResult> =>
    request<ModelUpsertResult>(Methods.model.upsert, params),

  deleteModel: (modelPk: string): Promise<OkResult> =>
    request<OkResult>(Methods.model.delete, { model_pk: modelPk } satisfies ModelDeleteParams),

  /** 拉上游 `/models` 清单，用于「添加上游模型」时的提示。 */
  listUpstreamModels: (providerId: string): Promise<LlmListModelsResult> =>
    request<LlmListModelsResult>(Methods.llm.listModels, {
      provider_id: providerId
    } satisfies LlmListModelsParams),

  countTokens: (params: LlmCountTokensParams): Promise<LlmCountTokensResult> =>
    request<LlmCountTokensResult>(Methods.llm.countTokens, params),

  /** 发起流式对话：先拿 `{stream_id}` 回执，正文与结束经 `llm.*` 事件回来。 */
  chatStream: (params: LlmChatStreamParams): Promise<LlmChatStreamResult> =>
    request<LlmChatStreamResult>(Methods.llm.chatStream, params),

  cancelStream: (streamId: string): Promise<OkResult> =>
    request<OkResult>(Methods.llm.cancel, { stream_id: streamId } satisfies LlmCancelParams)
}
