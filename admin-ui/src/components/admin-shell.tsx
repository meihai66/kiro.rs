import { useState } from 'react'
import { Link, Outlet, useLocation } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, BarChart3, BrainCircuit, KeyRound, LogOut, Moon, Settings as SettingsIcon, Shield, Sun, Users } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { storage } from '@/lib/storage'

interface AdminShellProps {
  onLogout: () => void
}

const TABS = [
  { to: '/credentials', label: '凭据', icon: Users },
  { to: '/proxies', label: '代理池', icon: Shield },
  { to: '/api-keys', label: '密钥', icon: KeyRound },
  { to: '/model-coverage', label: '模型', icon: BrainCircuit },
  { to: '/stats', label: '统计', icon: BarChart3 },
  { to: '/error-logs', label: '错误日志', icon: AlertTriangle },
  { to: '/settings', label: '设置', icon: SettingsIcon },
]

export function AdminShell({ onLogout }: AdminShellProps) {
  const location = useLocation()
  const queryClient = useQueryClient()
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return document.documentElement.classList.contains('dark')
    }
    return false
  })

  const toggleDarkMode = () => {
    setDarkMode((d) => !d)
    document.documentElement.classList.toggle('dark')
  }

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  return (
    <div className="min-h-screen bg-background">
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between px-4 md:px-8">
          <div className="flex items-center gap-6">
            <div className="flex items-center">
              <span className="font-semibold">Kiro</span>
            </div>
            <nav className="flex items-center gap-1">
              {TABS.map((t) => {
                const Icon = t.icon
                const active = location.pathname.startsWith(t.to)
                return (
                  <Link key={t.to} to={t.to}>
                    <Button
                      variant={active ? 'secondary' : 'ghost'}
                      size="sm"
                      className="h-8"
                    >
                      <Icon className="h-4 w-4 mr-2" />
                      {t.label}
                    </Button>
                  </Link>
                )
              })}
            </nav>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="icon" onClick={toggleDarkMode}>
              {darkMode ? (
                <Sun className="h-5 w-5" />
              ) : (
                <Moon className="h-5 w-5" />
              )}
            </Button>
            <Button variant="ghost" size="icon" onClick={handleLogout}>
              <LogOut className="h-5 w-5" />
            </Button>
          </div>
        </div>
      </header>

      <main className="container mx-auto px-4 md:px-8 py-6">
        <Outlet />
      </main>
    </div>
  )
}
