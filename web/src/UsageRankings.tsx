import { useId } from 'react';
import { ArrowDown, ArrowUpDown } from 'lucide-react';
import type { UsageRanking } from './api';
import { useQueryState } from './navigation';
import { TokenCount } from './TokenCount';
import { formatDuration } from './RequestRecords';
import { cacheRate } from './UsageTrend';

const number = new Intl.NumberFormat('zh-CN');
const percent = new Intl.NumberFormat('zh-CN', { style: 'percent', maximumFractionDigits: 1 });

export function UsageRankings({
  accounts,
  keys,
  models,
  loading,
  error,
}: {
  accounts: UsageRanking[];
  keys: UsageRanking[];
  models: UsageRanking[];
  loading: boolean;
  error: boolean;
}) {
  const headingId = useId();
  const noteId = useId();
  const [usageBy, setUsageBy] = useQueryState('usageBy', 'key');
  const [usageSort, setUsageSort] = useQueryState('usageSort', 'tokens');
  const byAccount = usageBy === 'account';
  const byModel = usageBy === 'model';
  const byRequests = usageSort === 'requests';
  const sortLabel = byRequests ? '请求次数' : '总 Token';
  const entity = byModel ? '模型' : byAccount ? '账户' : 'API 密钥';
  const title = `${entity}用量`;
  const rows = (byModel ? models : byAccount ? accounts : keys).toSorted(
    (a, b) =>
      (byRequests
        ? b.requestCount - a.requestCount
        : (b.totalTokens ?? -1) - (a.totalTokens ?? -1)) || a.id.localeCompare(b.id),
  );
  const totalRequests = rows.reduce((sum, row) => sum + row.requestCount, 0);
  const totalTokens = rows.reduce((sum, row) => sum + (row.totalTokens ?? 0), 0);
  const recordLink = (id: string) => {
    const params = new URLSearchParams(location.search);
    params.set('view', 'requests');
    params.delete('page');
    params.delete('tab');
    const field = byModel ? 'model' : byAccount ? 'accountId' : 'keyId';
    if (byAccount) params.delete('accountName');
    if (!byModel && !byAccount) params.delete('keyName');
    params.set(field, id);
    if (byModel) params.set('kind', 'inference');
    return `?${params}`;
  };
  return (
    <section className="panel usage-ranking" aria-labelledby={headingId} aria-busy={loading}>
      <div className="usage-ranking-heading">
        <div>
          <h2 id={headingId}>用量排行</h2>
          <p>
            按{entity}聚合
            <span className="usage-ranking-count">{number.format(rows.length)} 个</span>
          </p>
        </div>
        <div className="usage-switch" role="group" aria-label="用量聚合方式">
          <button type="button" aria-pressed={byAccount} onClick={() => setUsageBy('account')}>
            账户
          </button>
          <button
            type="button"
            aria-pressed={!byAccount && !byModel}
            onClick={() => setUsageBy('key')}
          >
            API 密钥
          </button>
          <button type="button" aria-pressed={byModel} onClick={() => setUsageBy('model')}>
            模型
          </button>
        </div>
      </div>
      {rows.length ? (
        <div
          key={entity}
          className="usage-ranking-scroll"
          tabIndex={0}
          role="region"
          aria-label={`${title}，可滚动`}
        >
          <table className="usage-ranking-table" aria-describedby={noteId}>
            <caption className="visually-hidden">
              {title}，按{sortLabel}降序排列
            </caption>
            <thead>
              <tr>
                <th scope="col" className="usage-identity-heading">
                  {entity}
                </th>
                <th
                  scope="col"
                  className="usage-sortable"
                  aria-sort={byRequests ? 'descending' : undefined}
                >
                  <button
                    type="button"
                    className="usage-sort-button"
                    aria-pressed={byRequests}
                    title="按请求次数降序排列"
                    onClick={() => setUsageSort('requests')}
                  >
                    请求次数
                    {byRequests ? (
                      <ArrowDown size={13} aria-hidden="true" />
                    ) : (
                      <ArrowUpDown size={13} aria-hidden="true" />
                    )}
                  </button>
                </th>
                <th scope="col">输入 Token</th>
                <th scope="col">输出 Token</th>
                <th scope="col">失败</th>
                <th scope="col">平均响应</th>
                <th scope="col">缓存命中率</th>
                <th
                  scope="col"
                  className="usage-sortable"
                  aria-sort={byRequests ? undefined : 'descending'}
                >
                  <button
                    type="button"
                    className="usage-sort-button"
                    aria-pressed={!byRequests}
                    title="按用量降序排列"
                    onClick={() => setUsageSort('tokens')}
                  >
                    总 Token
                    {byRequests ? (
                      <ArrowUpDown size={13} aria-hidden="true" />
                    ) : (
                      <ArrowDown size={13} aria-hidden="true" />
                    )}
                  </button>
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <tr key={row.id}>
                  <th scope="row" className="usage-identity">
                    <strong title={row.name ?? undefined}>
                      {row.id ? (
                        <a
                          className="usage-identity-link"
                          href={recordLink(row.id)}
                          title={`查看${row.name ?? entity}的请求记录`}
                        >
                          {row.name ?? `未命名${entity}`}
                        </a>
                      ) : (
                        `未知${entity}`
                      )}
                    </strong>
                    {!byModel ? (
                      <small title={row.id}>
                        {row.id.slice(0, 8)}…{row.id.slice(-4)}
                      </small>
                    ) : null}
                  </th>
                  <td className="usage-requests">
                    <strong>{number.format(row.requestCount)}</strong>
                    <small>
                      {percent.format(totalRequests ? row.requestCount / totalRequests : 0)}
                    </small>
                  </td>
                  <td>
                    <TokenCount value={row.inputTokens} />
                  </td>
                  <td>
                    <TokenCount value={row.outputTokens} />
                  </td>
                  <td
                    className={
                      row.failedRequests ? 'usage-failures has-failures' : 'usage-failures'
                    }
                  >
                    {number.format(row.failedRequests)}
                  </td>
                  <td className="usage-performance">
                    <span title="包含排队时间">
                      首响 {formatDuration(row.averageFirstResponseMs)}
                    </span>
                    <small title="不含排队时间，从开始执行到首个有效输出">
                      模型首响 {formatDuration(row.averageModelFirstResponseMs)}
                    </small>
                    <small>总耗时 {formatDuration(row.averageTotalMs)}</small>
                  </td>
                  <td className="usage-cache">
                    <strong>{cacheRate(row.cacheHitRate)}</strong>
                    <small>{number.format(row.cacheKnownRequests)} 次缓存用量已知</small>
                  </td>
                  <td className="usage-total">
                    <strong>
                      <TokenCount value={row.totalTokens} />
                    </strong>
                    <small>
                      {row.totalTokens === null
                        ? '用量未知'
                        : totalTokens
                          ? `${percent.format(row.totalTokens / totalTokens)} 用量占比`
                          : '0% 用量占比'}
                    </small>
                    <small className="usage-known">
                      {number.format(row.usageKnownRequests)} / {number.format(row.requestCount)}{' '}
                      次用量已知
                    </small>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="usage-ranking-empty">
          {loading
            ? `正在读取${title}…`
            : error
              ? `${title}暂时无法读取，请刷新重试。`
              : `当前筛选范围没有${entity}请求记录。`}
        </p>
      )}
      <details className="usage-ranking-help">
        <summary>统计口径与说明</summary>
        <p className="usage-ranking-note" id={noteId}>
          按{sortLabel}降序排列。总 Token = 输入 + 输出（含缓存）；未知用量不计为零。
          响应与缓存仅统计模型请求的已知样本，命中率按输入 Token 加权。点击名称查看请求记录。
        </p>
      </details>
    </section>
  );
}
