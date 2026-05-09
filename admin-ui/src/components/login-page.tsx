import { useState, useEffect } from 'react'
import axios from 'axios'
import { KeyRound } from 'lucide-react'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'

interface LoginPageProps {
  onLogin: (apiKey: string) => void
}

export function LoginPage({ onLogin }: LoginPageProps) {
  const [apiKey, setApiKey] = useState('')
  const [verifying, setVerifying] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    // 从 storage 读取保存的 API Key
    const savedKey = storage.getApiKey()
    if (savedKey) {
      setApiKey(savedKey)
    }
  }, [])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    const key = apiKey.trim()
    if (!key || verifying) return

    setVerifying(true)
    setError(null)
    try {
      // 用输入的 key 实际请求一次 admin API,验证密钥是否有效
      const resp = await axios.get('/api/admin/stats/summary', {
        headers: { 'x-api-key': key },
        validateStatus: () => true,
      })
      if (resp.status === 401 || resp.status === 403) {
        setError('Admin API Key 错误')
        return
      }
      if (resp.status >= 200 && resp.status < 300) {
        storage.setApiKey(key)
        onLogin(key)
        return
      }
      setError(`服务异常 (HTTP ${resp.status})`)
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err)
      setError(`无法连接到服务: ${msg}`)
    } finally {
      setVerifying(false)
    }
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full bg-primary/10">
            <KeyRound className="h-6 w-6 text-primary" />
          </div>
          <CardTitle className="text-2xl">Kiro Admin</CardTitle>
          <CardDescription>
            请输入 Admin API Key 以访问管理面板
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Input
                type="password"
                placeholder="Admin API Key"
                value={apiKey}
                onChange={(e) => {
                  setApiKey(e.target.value)
                  if (error) setError(null)
                }}
                className="text-center"
                autoFocus
                disabled={verifying}
              />
              {error && (
                <p className="text-sm text-destructive text-center">{error}</p>
              )}
            </div>
            <Button type="submit" className="w-full" disabled={!apiKey.trim() || verifying}>
              {verifying ? '验证中...' : '登录'}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
