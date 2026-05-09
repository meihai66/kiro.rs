import axios from 'axios'
import { storage } from '@/lib/storage'

export const adminApi = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

adminApi.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) config.headers['x-api-key'] = apiKey
  return config
})

adminApi.interceptors.response.use(
  (resp) => resp,
  (error) => {
    if (error?.response?.status === 401) {
      storage.removeApiKey()
      window.location.reload()
    }
    return Promise.reject(error)
  },
)
