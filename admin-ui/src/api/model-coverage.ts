import { adminApi as api } from '@/lib/admin-axios'

export interface AvailableModel {
  modelId?: string
  modelName?: string
  description?: string
  supportedInputTypes?: string[]
}

export interface ListCredentialModelsResponse {
  ok: boolean
  id: number
  /** 上游成功返回时存在 */
  data?: {
    defaultModel?: AvailableModel
    models?: AvailableModel[]
    [k: string]: unknown
  }
  /** 失败时返回 */
  error?: string
}

export async function listCredentialModels(
  credentialId: number
): Promise<ListCredentialModelsResponse> {
  const { data } = await api.post<ListCredentialModelsResponse>(
    `/credentials/${credentialId}/list-models`
  )
  return data
}
