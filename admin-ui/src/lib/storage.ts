const API_KEY_STORAGE_KEY = 'adminApiKey'
const REFRESH_INTERVAL_KEY = 'kiro-admin:refresh-interval-secs'
const DEFAULT_REFRESH_INTERVAL_SECS = 2

export const storage = {
  getApiKey: () => localStorage.getItem(API_KEY_STORAGE_KEY),
  setApiKey: (key: string) => localStorage.setItem(API_KEY_STORAGE_KEY, key),
  removeApiKey: () => localStorage.removeItem(API_KEY_STORAGE_KEY),

  /** 表格 / 仪表盘自动刷新频率（秒），默认 2 秒 */
  getRefreshIntervalSecs: (): number => {
    const v = Number(localStorage.getItem(REFRESH_INTERVAL_KEY))
    if (!isFinite(v) || v <= 0) return DEFAULT_REFRESH_INTERVAL_SECS
    return Math.max(1, Math.min(300, v))
  },
  setRefreshIntervalSecs: (n: number) => {
    const clamped = Math.max(1, Math.min(300, Math.floor(n)))
    localStorage.setItem(REFRESH_INTERVAL_KEY, String(clamped))
  },
}
