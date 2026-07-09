import { adminApi as api } from '@/lib/admin-axios'
import type {
  ClearErrorLogsRequest,
  ClearErrorLogsResponse,
  ErrorLogDetail,
  ErrorLogKindStatsResponse,
  ErrorLogListResponse,
  ListErrorLogsParams,
} from '@/types/api'

export async function listErrorLogs(
  params: ListErrorLogsParams = {}
): Promise<ErrorLogListResponse> {
  const { data } = await api.get<ErrorLogListResponse>('/error-logs', { params })
  return data
}

export async function getErrorLog(id: number): Promise<ErrorLogDetail> {
  const { data } = await api.get<ErrorLogDetail>(`/error-logs/${id}`)
  return data
}

export async function deleteErrorLog(id: number): Promise<void> {
  await api.delete(`/error-logs/${id}`)
}

export async function clearErrorLogs(
  req: ClearErrorLogsRequest = {}
): Promise<ClearErrorLogsResponse> {
  const { data } = await api.post<ClearErrorLogsResponse>('/error-logs/clear', req)
  return data
}

// 各错误类型的累计次数 + 当前留存条数
export async function getErrorLogKindStats(): Promise<ErrorLogKindStatsResponse> {
  const { data } = await api.get<ErrorLogKindStatsResponse>('/error-logs/stats')
  return data
}
