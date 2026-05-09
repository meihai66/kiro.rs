import {
  type ColumnDef,
  type PaginationState,
  type RowSelectionState,
  type SortingState,
  flexRender,
  getCoreRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  useReactTable,
} from '@tanstack/react-table'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'

interface DataTableProps<TData> {
  columns: ColumnDef<TData, unknown>[]
  data: TData[]
  rowSelection?: RowSelectionState
  onRowSelectionChange?: (selection: RowSelectionState) => void
  getRowId?: (row: TData) => string
  pageSize?: number
  emptyText?: string
  /// 当前页可见行变化时回调（含分页+排序后的顺序），用于"按页内序号"操作
  onVisibleRowsChange?: (rows: TData[]) => void
  /// 用于跨数据刷新持久化分页状态的 storageKey（保存到 sessionStorage）。
  /// 不传则使用本地组件 state（每次组件 unmount 会丢失）。
  paginationStorageKey?: string
}

function loadPagination(
  key: string | undefined,
  defaultPageSize: number
): PaginationState {
  if (!key || typeof window === 'undefined') {
    return { pageIndex: 0, pageSize: defaultPageSize }
  }
  try {
    const raw = window.sessionStorage.getItem(key)
    if (!raw) return { pageIndex: 0, pageSize: defaultPageSize }
    const parsed = JSON.parse(raw) as Partial<PaginationState>
    return {
      pageIndex: Math.max(0, Number(parsed.pageIndex) || 0),
      pageSize: Math.max(1, Number(parsed.pageSize) || defaultPageSize),
    }
  } catch {
    return { pageIndex: 0, pageSize: defaultPageSize }
  }
}

export function DataTable<TData>({
  columns,
  data,
  rowSelection,
  onRowSelectionChange,
  getRowId,
  pageSize = 20,
  emptyText = '暂无数据',
  onVisibleRowsChange,
  paginationStorageKey,
}: DataTableProps<TData>) {
  const [sorting, setSorting] = useState<SortingState>([])
  const [pagination, setPagination] = useState<PaginationState>(() =>
    loadPagination(paginationStorageKey, pageSize)
  )

  // 持久化到 sessionStorage（仅在 key 提供时）
  useEffect(() => {
    if (!paginationStorageKey || typeof window === 'undefined') return
    try {
      window.sessionStorage.setItem(
        paginationStorageKey,
        JSON.stringify(pagination)
      )
    } catch {
      // ignore quota/serialize errors
    }
  }, [pagination, paginationStorageKey])

  const table = useReactTable({
    data,
    columns,
    state: {
      sorting,
      rowSelection: rowSelection ?? {},
      pagination,
    },
    enableRowSelection: !!onRowSelectionChange,
    onSortingChange: setSorting,
    onPaginationChange: setPagination,
    onRowSelectionChange: (updater) => {
      if (!onRowSelectionChange) return
      const next =
        typeof updater === 'function' ? updater(rowSelection ?? {}) : updater
      onRowSelectionChange(next)
    },
    getRowId,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    // 关键：TanStack Table v8 默认 data 引用变化就把 pageIndex 重置为 0；
    // refetch 每 2-30s 一次，会让用户停在的页全部跳回第 1 页。
    // 我们的"数据缩短时回缩 pageIndex"逻辑在下方 useEffect 里手动处理。
    autoResetPageIndex: false,
  })

  // 数据缩短时，把 pageIndex 拉回到合法范围内（避免显示"第 N / N 页"且空白）
  // 关键：data.length === 0 时不要 reset——首次挂载/refetch 短暂为空时
  // 会把从 sessionStorage 恢复出来的页码错打回 0
  useEffect(() => {
    if (data.length === 0) return
    const pageCount = Math.max(1, table.getPageCount())
    if (pagination.pageIndex >= pageCount) {
      setPagination((p) => ({ ...p, pageIndex: pageCount - 1 }))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data.length, pagination.pageSize])

  // 当前页可见行（分页 + 排序后）→ 通知调用方
  const visibleRows = table.getRowModel().rows
  useEffect(() => {
    if (!onVisibleRowsChange) return
    onVisibleRowsChange(visibleRows.map((r) => r.original))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    visibleRows.length,
    table.getState().pagination.pageIndex,
    table.getState().pagination.pageSize,
    sorting,
    data,
  ])

  // 表格内部滚动：高度 = 视口剩余 - 底部分页栏 - 边距
  // 不固定一个 calc 值，因为不同页面表格上方的过滤/标题占位高度不一样
  const scrollRef = useRef<HTMLDivElement>(null)
  const footerRef = useRef<HTMLDivElement>(null)
  const [scrollMaxHeight, setScrollMaxHeight] = useState<number | null>(null)

  useLayoutEffect(() => {
    if (typeof window === 'undefined') return
    const recalc = () => {
      const el = scrollRef.current
      if (!el) return
      const top = el.getBoundingClientRect().top
      const footerH = footerRef.current?.getBoundingClientRect().height ?? 0
      // 留 16px 给页面底部 padding
      const available = window.innerHeight - top - footerH - 16
      setScrollMaxHeight(Math.max(240, Math.floor(available)))
    }
    recalc()
    window.addEventListener('resize', recalc)
    window.addEventListener('scroll', recalc, { passive: true })
    const ro = new ResizeObserver(recalc)
    if (scrollRef.current) ro.observe(scrollRef.current)
    // 监听上方过滤/标题区高度变化（其会改变 scrollRef 的 top）
    if (scrollRef.current?.parentElement) {
      ro.observe(scrollRef.current.parentElement)
    }
    return () => {
      window.removeEventListener('resize', recalc)
      window.removeEventListener('scroll', recalc)
      ro.disconnect()
    }
  }, [])

  return (
    <div className="space-y-3">
      <div
        ref={scrollRef}
        className="rounded-md border overflow-auto"
        style={
          scrollMaxHeight ? { maxHeight: scrollMaxHeight } : undefined
        }
      >
        <Table>
          <TableHeader className="sticky top-0 z-10 bg-background">
            {table.getHeaderGroups().map((hg) => (
              <TableRow key={hg.id}>
                {hg.headers.map((header) => (
                  <TableHead
                    key={header.id}
                    colSpan={header.colSpan}
                    className="whitespace-nowrap bg-background"
                  >
                    {header.isPlaceholder
                      ? null
                      : flexRender(
                          header.column.columnDef.header,
                          header.getContext()
                        )}
                  </TableHead>
                ))}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {table.getRowModel().rows?.length ? (
              table.getRowModel().rows.map((row) => (
                <TableRow
                  key={row.id}
                  data-state={row.getIsSelected() && 'selected'}
                >
                  {row.getVisibleCells().map((cell) => (
                    <TableCell key={cell.id}>
                      {flexRender(
                        cell.column.columnDef.cell,
                        cell.getContext()
                      )}
                    </TableCell>
                  ))}
                </TableRow>
              ))
            ) : (
              <TableRow>
                <TableCell
                  colSpan={columns.length}
                  className="h-24 text-center text-muted-foreground"
                >
                  {emptyText}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>

      <div
        ref={footerRef}
        className="flex items-center justify-between text-sm text-muted-foreground"
      >
        <div>
          共 {table.getFilteredRowModel().rows.length} 条
          {table.getSelectedRowModel().rows.length > 0 && (
            <span className="ml-2">
              · 已选 {table.getSelectedRowModel().rows.length}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          <PageNumbers
            pageIndex={table.getState().pagination.pageIndex}
            pageCount={Math.max(table.getPageCount(), 1)}
            onChange={(idx) => table.setPageIndex(idx)}
          />
          <select
            className="ml-2 h-8 rounded border bg-background px-2 text-xs"
            value={table.getState().pagination.pageSize}
            onChange={(e) => table.setPageSize(Number(e.target.value))}
          >
            {[20, 50, 100, 200, 500, 1000].map((s) => (
              <option key={s} value={s}>
                每页 {s}
              </option>
            ))}
          </select>
        </div>
      </div>
    </div>
  )
}

/**
 * 页码导航：「< 1 2 3 4 5 ... N >」 形式。
 * 当前页之外用 ghost 按钮，当前页用 secondary 高亮。
 * 总页数 ≤ 7 全部铺开；多于 7 时按"首尾 + 当前 ±2 + 省略号"折叠。
 */
function PageNumbers({
  pageIndex,
  pageCount,
  onChange,
}: {
  pageIndex: number
  pageCount: number
  onChange: (idx: number) => void
}) {
  const cur = pageIndex + 1
  const last = pageCount
  const items: Array<number | 'gap'> = []
  if (pageCount <= 7) {
    for (let i = 1; i <= last; i++) items.push(i)
  } else {
    items.push(1)
    if (cur - 2 > 2) items.push('gap')
    const start = Math.max(2, cur - 2)
    const end = Math.min(last - 1, cur + 2)
    for (let i = start; i <= end; i++) items.push(i)
    if (cur + 2 < last - 1) items.push('gap')
    items.push(last)
  }
  return (
    <div className="flex items-center gap-1">
      <Button
        variant="ghost"
        size="sm"
        className="h-8 w-8 p-0"
        onClick={() => onChange(Math.max(0, pageIndex - 1))}
        disabled={pageIndex === 0}
      >
        ‹
      </Button>
      {items.map((it, i) =>
        it === 'gap' ? (
          <span
            key={`gap-${i}`}
            className="px-1 text-xs text-muted-foreground select-none"
          >
            …
          </span>
        ) : (
          <Button
            key={it}
            variant={it === cur ? 'secondary' : 'ghost'}
            size="sm"
            className="h-8 min-w-8 px-2 text-xs"
            onClick={() => onChange(it - 1)}
          >
            {it}
          </Button>
        )
      )}
      <Button
        variant="ghost"
        size="sm"
        className="h-8 w-8 p-0"
        onClick={() => onChange(Math.min(pageCount - 1, pageIndex + 1))}
        disabled={pageIndex >= pageCount - 1}
      >
        ›
      </Button>
    </div>
  )
}
