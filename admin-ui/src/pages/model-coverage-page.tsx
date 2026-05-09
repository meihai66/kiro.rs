import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Plus, RefreshCw, Trash2, X } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { useCredentials } from '@/hooks/use-credentials'
import { listCredentialModels } from '@/api/model-coverage'

const STORAGE_KEY = 'kiro-admin:model-coverage:targets'

const DEFAULT_TARGETS: string[] = [
  'claude-opus-4.7',
  'claude-opus-4.6',
  'claude-sonnet-4.6',
  'claude-opus-4.5',
  'claude-sonnet-4.5',
  'claude-sonnet-4',
  'claude-haiku-4.5',
]

function loadTargets(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return DEFAULT_TARGETS.slice()
    const arr = JSON.parse(raw)
    if (Array.isArray(arr) && arr.every((s) => typeof s === 'string')) {
      return arr
    }
  } catch {
    /* ignore */
  }
  return DEFAULT_TARGETS.slice()
}

function saveTargets(arr: string[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(arr))
  } catch {
    /* ignore */
  }
}

interface Failure {
  id: number
  email: string
  error: string
}

export function ModelCoveragePage() {
  const { data: credsData } = useCredentials()
  const credentials = credsData?.credentials ?? []

  // 目标模型列表（localStorage 持久化）
  const [targets, setTargets] = useState<string[]>(() => loadTargets())
  const [newModelInput, setNewModelInput] = useState('')

  // 选中要测试的凭据 id（默认全部）
  const [selectedCredIds, setSelectedCredIds] = useState<Set<number>>(new Set())

  // 检测进行中
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState({ current: 0, total: 0 })
  const [supported, setSupported] = useState<Map<number, Set<string>>>(new Map())
  const [failures, setFailures] = useState<Failure[]>([])
  const [cancelFlag, setCancelFlag] = useState(false)

  useEffect(() => {
    saveTargets(targets)
  }, [targets])

  // 默认勾选所有可用凭据
  useEffect(() => {
    if (credentials.length === 0 || selectedCredIds.size > 0) return
    setSelectedCredIds(new Set(credentials.map((c) => c.id)))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [credsData])

  const addModel = () => {
    const m = newModelInput.trim()
    if (!m) return
    if (targets.includes(m)) {
      toast.info('该模型已在列表中')
      return
    }
    setTargets([...targets, m])
    setNewModelInput('')
  }

  const removeModel = (m: string) => {
    setTargets(targets.filter((x) => x !== m))
  }

  const resetToDefault = () => {
    if (!confirm('恢复默认模型列表？将覆盖你当前的列表。')) return
    setTargets(DEFAULT_TARGETS.slice())
  }

  const toggleCred = (id: number) => {
    const next = new Set(selectedCredIds)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    setSelectedCredIds(next)
  }

  const selectAllCreds = () => {
    setSelectedCredIds(new Set(credentials.map((c) => c.id)))
  }

  const clearCredSelection = () => {
    setSelectedCredIds(new Set())
  }

  const runCoverage = async () => {
    if (targets.length === 0) {
      toast.error('请先添加至少一个目标模型')
      return
    }
    if (selectedCredIds.size === 0) {
      toast.error('请先勾选要检测的凭据')
      return
    }
    setRunning(true)
    setCancelFlag(false)
    setSupported(new Map())
    setFailures([])
    const ids = Array.from(selectedCredIds)
    setProgress({ current: 0, total: ids.length })

    const supMap = new Map<number, Set<string>>()
    const failList: Failure[] = []

    for (let i = 0; i < ids.length; i++) {
      if (cancelFlag) break
      const id = ids[i]
      const cred = credentials.find((c) => c.id === id)
      try {
        const resp = await listCredentialModels(id)
        if (resp.ok && resp.data && Array.isArray(resp.data.models)) {
          const set = new Set<string>()
          resp.data.models.forEach((m) => {
            if (m && m.modelId) set.add(m.modelId)
          })
          supMap.set(id, set)
          setSupported(new Map(supMap))
        } else {
          failList.push({
            id,
            email: cred?.email || `#${id}`,
            error: resp.error || '请求失败',
          })
          setFailures([...failList])
        }
      } catch (e) {
        failList.push({
          id,
          email: cred?.email || `#${id}`,
          error: (e as Error).message,
        })
        setFailures([...failList])
      }
      setProgress({ current: i + 1, total: ids.length })
    }
    setRunning(false)
    if (cancelFlag) {
      toast.info('已取消')
    } else {
      toast.success(
        `检测完成：成功 ${supMap.size} / 失败 ${failList.length}`
      )
    }
  }

  const cancelCoverage = () => {
    setCancelFlag(true)
  }

  // 按模型聚合"哪些账号缺失"
  const grouped = useMemo(() => {
    return targets.map((modelId) => {
      const missing: { id: number; email: string }[] = []
      supported.forEach((modelSet, credId) => {
        if (!modelSet.has(modelId)) {
          const c = credentials.find((x) => x.id === credId)
          missing.push({ id: credId, email: c?.email || `#${credId}` })
        }
      })
      return { modelId, missing }
    })
  }, [targets, supported, credentials])

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-2xl font-semibold">模型检测</h1>
        <div className="flex items-center gap-2">
          {running ? (
            <Button size="sm" variant="destructive" onClick={cancelCoverage}>
              取消（{progress.current}/{progress.total}）
            </Button>
          ) : (
            <Button size="sm" onClick={runCoverage}>
              <RefreshCw className="h-4 w-4 mr-2" />
              开始检测
            </Button>
          )}
        </div>
      </div>

      {/* 目标模型 */}
      <Card className="mb-4">
        <CardHeader className="pb-2">
          <CardTitle className="text-base flex items-center justify-between">
            <span>目标模型（{targets.length} 个，本地保存）</span>
            <Button size="sm" variant="ghost" onClick={resetToDefault}>
              恢复默认
            </Button>
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex flex-wrap gap-1">
            {targets.length === 0 ? (
              <span className="text-xs text-muted-foreground">
                列表为空，请添加目标模型
              </span>
            ) : (
              targets.map((m) => (
                <Badge
                  key={m}
                  variant="outline"
                  className="font-mono text-xs gap-1 pr-1"
                >
                  {m}
                  <Button
                    size="icon"
                    variant="ghost"
                    className="h-4 w-4 hover:bg-destructive/20"
                    onClick={() => removeModel(m)}
                  >
                    <X className="h-3 w-3" />
                  </Button>
                </Badge>
              ))
            )}
          </div>
          <div className="flex items-center gap-2">
            <Input
              placeholder="如 claude-opus-4.7"
              value={newModelInput}
              onChange={(e) => setNewModelInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') addModel()
              }}
              className="h-8 max-w-xs text-xs font-mono"
            />
            <Button size="sm" variant="outline" onClick={addModel}>
              <Plus className="h-4 w-4 mr-1" />
              添加
            </Button>
            <span className="text-xs text-muted-foreground">
              （回车快速添加；模型 ID 形如 claude-opus-4.x / claude-sonnet-4.x）
            </span>
          </div>
        </CardContent>
      </Card>

      {/* 凭据选择 */}
      <Card className="mb-4">
        <CardHeader className="pb-2">
          <CardTitle className="text-base flex items-center justify-between">
            <span>凭据范围（已选 {selectedCredIds.size} / 共 {credentials.length}）</span>
            <div className="flex gap-1">
              <Button size="sm" variant="ghost" onClick={selectAllCreds}>
                全选
              </Button>
              <Button size="sm" variant="ghost" onClick={clearCredSelection}>
                取消
              </Button>
            </div>
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid gap-1 grid-cols-2 md:grid-cols-3 lg:grid-cols-4 max-h-64 overflow-auto">
            {credentials.map((c) => (
              <label
                key={c.id}
                className="flex items-center gap-2 px-2 py-1 rounded hover:bg-muted/50 cursor-pointer text-xs"
              >
                <Checkbox
                  checked={selectedCredIds.has(c.id)}
                  onCheckedChange={() => toggleCred(c.id)}
                />
                <span className="font-mono">#{c.id}</span>
                <span className="text-muted-foreground truncate flex-1 min-w-0">
                  {c.email || '—'}
                </span>
                {c.disabled && (
                  <Badge variant="outline" className="text-[10px]">
                    禁用
                  </Badge>
                )}
              </label>
            ))}
          </div>
        </CardContent>
      </Card>

      {/* 进度 */}
      {(running || progress.total > 0) && (
        <Card className="mb-4">
          <CardContent className="py-3">
            <div className="flex items-center gap-3">
              <span className="text-sm">
                进度：{progress.current} / {progress.total}
              </span>
              <div className="flex-1 h-2 bg-muted rounded-full overflow-hidden">
                <div
                  className="h-full bg-emerald-500 transition-all"
                  style={{
                    width: `${
                      progress.total > 0
                        ? (progress.current / progress.total) * 100
                        : 0
                    }%`,
                  }}
                />
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* 检测结果：按模型聚合 */}
      {supported.size > 0 && (
        <div className="space-y-3">
          {grouped.map(({ modelId, missing }) => (
            <Card key={modelId}>
              <CardHeader className="pb-2">
                <CardTitle className="text-base flex items-center gap-2">
                  <span className="font-mono">{modelId}</span>
                  {missing.length === 0 ? (
                    <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                      所有账号支持
                    </Badge>
                  ) : (
                    <Badge variant="destructive">
                      缺失 {missing.length}
                    </Badge>
                  )}
                </CardTitle>
              </CardHeader>
              <CardContent>
                {missing.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    在所有成功返回的账号上都支持
                  </p>
                ) : (
                  <div className="grid gap-1 grid-cols-2 md:grid-cols-3 lg:grid-cols-4">
                    {missing.map((m) => (
                      <div
                        key={m.id}
                        className="flex items-center gap-2 px-2 py-1 rounded border bg-muted/30 text-xs"
                      >
                        <span className="font-mono">#{m.id}</span>
                        <span className="text-muted-foreground truncate flex-1">
                          {m.email}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* 失败列表 */}
      {failures.length > 0 && (
        <Card className="mt-4">
          <CardHeader className="pb-2">
            <CardTitle className="text-base flex items-center gap-2">
              <Trash2 className="h-4 w-4 text-red-500" />
              请求失败 {failures.length} 个
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 max-h-48 overflow-auto">
            {failures.map((f) => (
              <div
                key={f.id}
                className="flex items-center gap-2 text-xs border-b pb-1"
              >
                <span className="font-mono">#{f.id}</span>
                <span className="text-muted-foreground truncate max-w-[180px]">
                  {f.email}
                </span>
                <span className="text-red-500 truncate flex-1" title={f.error}>
                  {f.error}
                </span>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </>
  )
}
