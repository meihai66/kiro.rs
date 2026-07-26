import { adminApi as api } from '@/lib/admin-axios'
import type {
  ClearErrorLogsResponse,
  KeySeenListResponse,
  KeyOnboardLogListResponse,
  KeyPollConfigResponse,
  KeyPollLogDetail,
  KeyPollLogListResponse,
  ListKeyPollLogsParams,
  PollOutcome,
  UpdateKeyPollConfigRequest,
} from '@/types/api'

export async function getKeyPollConfig(): Promise<KeyPollConfigResponse> {
  const { data } = await api.get<KeyPollConfigResponse>('/key-poll/config')
  return data
}

export async function updateKeyPollConfig(
  req: UpdateKeyPollConfigRequest
): Promise<KeyPollConfigResponse> {
  const { data } = await api.post<KeyPollConfigResponse>('/key-poll/config', req)
  return data
}

/** 立即执行一次上号；dryRun 只拉取比对不导入 */
export async function runKeyPoll(dryRun = false): Promise<PollOutcome> {
  const { data } = await api.post<PollOutcome>('/key-poll/run', { dryRun })
  return data
}

export async function listKeyPollLogs(
  params: ListKeyPollLogsParams = {}
): Promise<KeyPollLogListResponse> {
  const { data } = await api.get<KeyPollLogListResponse>('/key-poll/logs', { params })
  return data
}

export async function getKeyPollLog(id: number): Promise<KeyPollLogDetail> {
  const { data } = await api.get<KeyPollLogDetail>(`/key-poll/logs/${id}`)
  return data
}

export async function clearKeyPollLogs(): Promise<ClearErrorLogsResponse> {
  const { data } = await api.post<ClearErrorLogsResponse>('/key-poll/logs/clear', {})
  return data
}

export async function listKeyOnboardLogs(
  params: ListKeyPollLogsParams = {}
): Promise<KeyOnboardLogListResponse> {
  const { data } = await api.get<KeyOnboardLogListResponse>('/key-poll/onboard-logs', { params })
  return data
}

export async function clearKeyOnboardLogs(): Promise<ClearErrorLogsResponse> {
  const { data } = await api.post<ClearErrorLogsResponse>('/key-poll/onboard-logs/clear', {})
  return data
}

export async function listKeySeen(
  params: ListKeyPollLogsParams = {}
): Promise<KeySeenListResponse> {
  const { data } = await api.get<KeySeenListResponse>('/key-poll/seen', { params })
  return data
}

/** 删除一条去重记录：该 Key 下次轮询会重新尝试上号 */
export async function deleteKeySeen(hash: string): Promise<void> {
  await api.delete(`/key-poll/seen/${hash}`)
}

export async function clearKeySeen(onlyFailed: boolean): Promise<ClearErrorLogsResponse> {
  const { data } = await api.post<ClearErrorLogsResponse>('/key-poll/seen/clear', { onlyFailed })
  return data
}
