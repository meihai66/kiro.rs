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
import type { ReactNode } from 'react'
import { useEffect, useState } from 'react'
import {
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  ChevronsUpDown,
  Rows3,
} from 'lucide-react'
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
  /// 渲染到顶部工具栏「分页选择」右侧的额外节点（搜索框、视图切换等）
  headerSlot?: ReactNode
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
  headerSlot,
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

  return (
    <div className="flex flex-col flex-1 min-h-0 gap-2">
      {/* 顶部工具栏：行计数 + 分页 + 每页大小 + 自定义 slot */}
      <div className="flex items-center justify-between gap-3 text-sm shrink-0">
        <div className="flex items-baseline gap-2 text-muted-foreground">
          <span className="text-xs">共</span>
          <span className="font-mono font-semibold tabular-nums text-foreground">
            {table.getFilteredRowModel().rows.length}
          </span>
          <span className="text-xs">条</span>
          {table.getSelectedRowModel().rows.length > 0 && (
            <>
              <span className="h-3 w-px bg-border self-center" />
              <span className="text-xs">已选</span>
              <span className="font-mono font-semibold tabular-nums text-emerald-600 dark:text-emerald-400">
                {table.getSelectedRowModel().rows.length}
              </span>
            </>
          )}
        </div>
        <div className="flex items-center gap-2">
          <PageNumbers
            pageIndex={table.getState().pagination.pageIndex}
            pageCount={Math.max(table.getPageCount(), 1)}
            onChange={(idx) => table.setPageIndex(idx)}
          />
          <div className="inline-flex h-8 items-center gap-1 rounded-md border bg-background px-2 text-xs shadow-sm">
            <Rows3 className="h-3.5 w-3.5 text-muted-foreground" />
            <select
              className="h-full bg-transparent text-xs border-0 focus:outline-none focus:ring-0 cursor-pointer pr-1 tabular-nums"
              value={table.getState().pagination.pageSize}
              onChange={(e) => table.setPageSize(Number(e.target.value))}
            >
              {[20, 50, 100, 200, 500, 1000].map((s) => (
                <option key={s} value={s}>
                  {s}/页
                </option>
              ))}
            </select>
          </div>
          {headerSlot}
        </div>
      </div>

      <div
        className="rounded-md border overflow-auto flex-1 min-h-0"
      >
        <Table>
          <TableHeader className="sticky top-0 z-10 bg-background">
            {table.getHeaderGroups().map((hg) => (
              <TableRow key={hg.id}>
                {hg.headers.map((header) => {
                  if (header.isPlaceholder) {
                    return (
                      <TableHead
                        key={header.id}
                        colSpan={header.colSpan}
                        className="whitespace-nowrap bg-background"
                      />
                    )
                  }
                  const content = flexRender(
                    header.column.columnDef.header,
                    header.getContext()
                  )
                  const canSort = header.column.getCanSort()
                  if (!canSort) {
                    return (
                      <TableHead
                        key={header.id}
                        colSpan={header.colSpan}
                        className="whitespace-nowrap bg-background"
                      >
                        {content}
                      </TableHead>
                    )
                  }
                  const sortDir = header.column.getIsSorted()
                  return (
                    <TableHead
                      key={header.id}
                      colSpan={header.colSpan}
                      className="whitespace-nowrap bg-background"
                    >
                      <button
                        type="button"
                        onClick={header.column.getToggleSortingHandler()}
                        className="inline-flex items-center gap-1 hover:text-foreground transition-colors select-none"
                        title="点击切换排序"
                      >
                        <span>{content}</span>
                        {sortDir === 'asc' ? (
                          <ChevronUp className="h-3 w-3 text-foreground" />
                        ) : sortDir === 'desc' ? (
                          <ChevronDown className="h-3 w-3 text-foreground" />
                        ) : (
                          <ChevronsUpDown className="h-3 w-3 opacity-40" />
                        )}
                      </button>
                    </TableHead>
                  )
                })}
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
    <div className="inline-flex h-8 items-center rounded-md border bg-background overflow-hidden shadow-sm">
      <button
        type="button"
        className="inline-flex h-full w-8 items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
        onClick={() => onChange(Math.max(0, pageIndex - 1))}
        disabled={pageIndex === 0}
        title="上一页"
      >
        <ChevronLeft className="h-4 w-4" />
      </button>
      {items.map((it, i) =>
        it === 'gap' ? (
          <span
            key={`gap-${i}`}
            className="px-1.5 text-xs text-muted-foreground select-none border-l"
          >
            …
          </span>
        ) : (
          <button
            key={it}
            type="button"
            onClick={() => onChange(it - 1)}
            className={
              'h-full min-w-8 px-2 text-xs border-l tabular-nums transition-colors ' +
              (it === cur
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'hover:bg-muted')
            }
          >
            {it}
          </button>
        )
      )}
      <button
        type="button"
        className="inline-flex h-full w-8 items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground transition-colors border-l disabled:opacity-30 disabled:cursor-not-allowed"
        onClick={() => onChange(Math.min(pageCount - 1, pageIndex + 1))}
        disabled={pageIndex >= pageCount - 1}
        title="下一页"
      >
        <ChevronRight className="h-4 w-4" />
      </button>
    </div>
  )
}
