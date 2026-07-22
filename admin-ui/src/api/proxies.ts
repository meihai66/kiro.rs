import { adminApi as api } from '@/lib/admin-axios'
import type {
  BatchProxyDeleteRequest,
  BatchProxyExtendRequest,
  BatchProxyResponse,
  BatchProxySlotsRequest,
  BatchProxyTestResponse,
  BatchProxyUnbindRequest,
  BindProxyRequest,
  ImportProxiesRequest,
  ImportProxiesResponse,
  ProxyAlertsResponse,
  ProxyListResponse,
  ProxyTestResult,
  SuccessResponse,
} from '@/types/api'

export async function getProxies(): Promise<ProxyListResponse> {
  const { data } = await api.get<ProxyListResponse>('/proxies')
  return data
}

export async function importProxies(
  req: ImportProxiesRequest
): Promise<ImportProxiesResponse> {
  const { data } = await api.post<ImportProxiesResponse>('/proxies/import', req)
  return data
}

export async function deleteProxy(
  id: string,
  force = false
): Promise<{ success: boolean; freedCredentialIds: number[] }> {
  const { data } = await api.delete<{
    success: boolean
    freedCredentialIds: number[]
  }>(`/proxies/${encodeURIComponent(id)}${force ? '?force=true' : ''}`)
  return data
}

export async function batchDeleteProxies(
  req: BatchProxyDeleteRequest
): Promise<BatchProxyResponse> {
  const { data } = await api.post<BatchProxyResponse>(
    '/proxies/batch/delete',
    req
  )
  return data
}

export async function batchUnbindProxies(
  req: BatchProxyUnbindRequest
): Promise<BatchProxyResponse> {
  const { data } = await api.post<BatchProxyResponse>(
    '/proxies/batch/unbind',
    req
  )
  return data
}

export async function batchSetProxySlots(
  req: BatchProxySlotsRequest
): Promise<BatchProxyResponse> {
  const { data } = await api.post<BatchProxyResponse>(
    '/proxies/batch/slots',
    req
  )
  return data
}

export async function batchExtendProxies(
  req: BatchProxyExtendRequest
): Promise<BatchProxyResponse> {
  const { data } = await api.post<BatchProxyResponse>(
    '/proxies/batch/extend',
    req
  )
  return data
}

export async function setProxyDisabled(
  id: string,
  disabled: boolean
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/proxies/${encodeURIComponent(id)}/set-disabled`,
    { disabled }
  )
  return data
}

export async function batchResetProxyDisabled(
  ids: string[]
): Promise<BatchProxyResponse> {
  const { data } = await api.post<BatchProxyResponse>(
    '/proxies/batch/reset-disabled',
    { ids }
  )
  return data
}

export async function testProxy(id: string): Promise<ProxyTestResult> {
  const { data } = await api.post<ProxyTestResult>(
    `/proxies/${encodeURIComponent(id)}/test`
  )
  return data
}

export async function batchTestProxies(
  ids: string[]
): Promise<BatchProxyTestResponse> {
  const { data } = await api.post<BatchProxyTestResponse>(
    '/proxies/batch/test',
    { ids }
  )
  return data
}

export async function rotateProxiesNow(): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/proxies/rotate')
  return data
}

export async function getProxyAlerts(): Promise<ProxyAlertsResponse> {
  const { data } = await api.get<ProxyAlertsResponse>('/proxies/alerts')
  return data
}

export async function bindCredentialProxy(
  credentialId: number,
  req: BindProxyRequest
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${credentialId}/bind-proxy`,
    req
  )
  return data
}

export async function unbindCredentialProxy(
  credentialId: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${credentialId}/unbind-proxy`
  )
  return data
}
