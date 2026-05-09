import { useState, useEffect } from 'react'
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import { storage } from '@/lib/storage'
import { LoginPage } from '@/components/login-page'
import { AdminShell } from '@/components/admin-shell'
import { CredentialsPage } from '@/pages/credentials-page'
import { ApiKeysPage } from '@/pages/api-keys-page'
import { ErrorLogsPage } from '@/pages/error-logs-page'
import { ModelCoveragePage } from '@/pages/model-coverage-page'
import { ProxiesPage } from '@/pages/proxies-page'
import { SettingsPage } from '@/pages/settings-page'
import { StatsPage } from '@/pages/stats-page'
import { Toaster } from '@/components/ui/sonner'

function App() {
  const [isLoggedIn, setIsLoggedIn] = useState(false)

  useEffect(() => {
    if (storage.getApiKey()) {
      setIsLoggedIn(true)
    }
  }, [])

  const handleLogin = () => setIsLoggedIn(true)
  const handleLogout = () => setIsLoggedIn(false)

  if (!isLoggedIn) {
    return (
      <>
        <LoginPage onLogin={handleLogin} />
        <Toaster position="top-right" />
      </>
    )
  }

  return (
    <>
      <BrowserRouter basename="/admin">
        <Routes>
          <Route element={<AdminShell onLogout={handleLogout} />}>
            <Route path="/" element={<Navigate to="/stats" replace />} />
            <Route path="/credentials" element={<CredentialsPage />} />
            <Route path="/proxies" element={<ProxiesPage />} />
            <Route path="/api-keys" element={<ApiKeysPage />} />
            <Route path="/model-coverage" element={<ModelCoveragePage />} />
            <Route path="/stats" element={<StatsPage />} />
            <Route path="/error-logs" element={<ErrorLogsPage />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/stats" replace />} />
          </Route>
        </Routes>
      </BrowserRouter>
      <Toaster position="top-right" />
    </>
  )
}

export default App
