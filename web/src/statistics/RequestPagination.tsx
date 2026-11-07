import type { SetStateAction } from 'react';
import { ArrowLeft, ArrowRight } from 'lucide-react';

export function RequestPagination({
  page,
  pageSize,
  displayedPage,
  total,
  loading,
  error,
  setPage,
  setPageSize,
}: {
  page: number;
  pageSize: number;
  displayedPage?: number;
  total: number;
  loading: boolean;
  error: boolean;
  setPage: (value: SetStateAction<number>) => void;
  setPageSize: (value: SetStateAction<number>) => void;
}) {
  return (
    <nav className="pagination" aria-label="请求记录分页">
      <label>
        <span className="visually-hidden">每页条数</span>
        <select
          aria-label="每页条数"
          value={pageSize}
          disabled={loading}
          onChange={(event) => {
            setPageSize(Number(event.target.value));
            setPage(1);
          }}
        >
          <option value={20}>20 条</option>
          <option value={50}>50 条</option>
          <option value={100}>100 条</option>
        </select>
      </label>
      <span aria-live="polite">
        第 {displayedPage ?? page} / {Math.max(1, Math.ceil(total / pageSize))} 页 · 共 {total} 条
      </span>
      <button
        className="button secondary"
        aria-label="上一页"
        title="上一页"
        disabled={page <= 1 || loading || !!error}
        onClick={() => setPage((p) => p - 1)}
      >
        <ArrowLeft size={15} aria-hidden="true" />
      </button>
      <button
        className="button secondary"
        aria-label="下一页"
        title="下一页"
        disabled={loading || !!error || page * pageSize >= total}
        onClick={() => setPage((p) => p + 1)}
      >
        <ArrowRight size={15} aria-hidden="true" />
      </button>
    </nav>
  );
}
