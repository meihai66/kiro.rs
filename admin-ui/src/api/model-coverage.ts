import axios from 'axios'
import { storage } from '@/lib/storage'

const api = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) config.headers['x-api-key'] = apiKey
  return config
})

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
