import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  ApiKeyItem,
  ApiKeyListResponse,
  CreateApiKeyRequest,
  SuccessResponse,
  UpdateApiKeyRequest,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) config.headers['x-api-key'] = apiKey
  return config
})

export async function listApiKeys(): Promise<ApiKeyListResponse> {
  const { data } = await api.get<ApiKeyListResponse>('/api-keys')
  return data
}

export async function createApiKey(
  req: CreateApiKeyRequest
): Promise<ApiKeyItem> {
  const { data } = await api.post<ApiKeyItem>('/api-keys', req)
  return data
}

export async function updateApiKey(
  id: number,
  req: UpdateApiKeyRequest
): Promise<SuccessResponse> {
  const { data } = await api.patch<SuccessResponse>(`/api-keys/${id}`, req)
  return data
}

export async function deleteApiKey(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/api-keys/${id}`)
  return data
}
