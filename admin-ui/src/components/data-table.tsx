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
import { useEffect, useState } from 'react'
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
  })

  // 数据缩短时，把 pageIndex 拉回到合法范围内（避免显示"第 N / N 页"且空白）
  useEffect(() => {
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
    <div className="space-y-3">
      <div className="rounded-md border">
        <Table>
          <TableHeader>
            {table.getHeaderGroups().map((hg) => (
              <TableRow key={hg.id}>
                {hg.headers.map((header) => (
                  <TableHead
                    key={header.id}
                    colSpan={header.colSpan}
                    className="whitespace-nowrap"
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

      <div className="flex items-center justify-between text-sm text-muted-foreground">
        <div>
          共 {table.getFilteredRowModel().rows.length} 条
          {table.getSelectedRowModel().rows.length > 0 && (
            <span className="ml-2">
              · 已选 {table.getSelectedRowModel().rows.length}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <span>
            第 {table.getState().pagination.pageIndex + 1} /{' '}
            {Math.max(table.getPageCount(), 1)} 页
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => table.previousPage()}
            disabled={!table.getCanPreviousPage()}
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => table.nextPage()}
            disabled={!table.getCanNextPage()}
          >
            下一页
          </Button>
          <select
            className="h-8 rounded border bg-background px-2 text-xs"
            value={table.getState().pagination.pageSize}
            onChange={(e) => table.setPageSize(Number(e.target.value))}
          >
            {[20, 50, 100, 200].map((s) => (
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
