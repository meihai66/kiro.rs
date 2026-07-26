import { adminApi as api } from '@/lib/admin-axios'
import type {
  ClearErrorLogsResponse,
  ListWebhookLogsParams,
  UpdateWebhookConfigRequest,
  WebhookConfigResponse,
  WebhookLogDetail,
  WebhookLogListResponse,
} from '@/types/api'

export async function getWebhookConfig(): Promise<WebhookConfigResponse> {
  const { data } = await api.get<WebhookConfigResponse>('/webhook/config')
  return data
}

export async function updateWebhookConfig(
  req: UpdateWebhookConfigRequest
): Promise<WebhookConfigResponse> {
  const { data } = await api.post<WebhookConfigResponse>('/webhook/config', req)
  return data
}

export async function listWebhookLogs(
  params: ListWebhookLogsParams = {}
): Promise<WebhookLogListResponse> {
  const { data } = await api.get<WebhookLogListResponse>('/webhook/logs', { params })
  return data
}

export async function getWebhookLog(id: number): Promise<WebhookLogDetail> {
  const { data } = await api.get<WebhookLogDetail>(`/webhook/logs/${id}`)
  return data
}

export async function deleteWebhookLog(id: number): Promise<void> {
  await api.delete(`/webhook/logs/${id}`)
}

export async function clearWebhookLogs(): Promise<ClearErrorLogsResponse> {
  const { data } = await api.post<ClearErrorLogsResponse>('/webhook/logs/clear', {})
  return data
}
