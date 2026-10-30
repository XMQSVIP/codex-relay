import { Activity, Coins, Gauge } from 'lucide-react';
import type { Statistics } from './api';
import { TokenCount } from './TokenCount';
import { formatDuration } from './RequestRecords';
import { UsageSparkline, cacheRate } from './UsageTrend';

const number = new Intl.NumberFormat('zh-CN');

export function UsageOverview({ data, loading }: { data: Statistics | null; loading: boolean }) {
  const summary = data?.summary;
  const unknownRequests = Math.max(
    0,
    (summary?.inferenceRequests ?? 0) - (summary?.usageKnownRequests ?? 0),
  );
  const now = Date.now();
  const buckets = data?.trend ?? [];
  const hasTokenUsage = buckets.some(
    (bucket) =>
      bucket.startedAt <= now && bucket.inferenceRequests > 0 && bucket.totalTokens !== null,
  );
  const unavailable = loading ? '—' : '未知';
  return (
    <section className="usage-summary" aria-label="模型用量概览" aria-busy={loading}>
      <div className="usage-primary-metrics">
        <div className="usage-metric-card trend-input">
          <div className="usage-metric-body">
            <div className="usage-metric-copy">
              <span className="usage-metric-label">
                <Activity size={17} aria-hidden="true" />
                模型请求
              </span>
              <strong>{summary ? number.format(summary.inferenceRequests) : '—'}</strong>
            </div>
            <UsageSparkline
              label="模型请求"
              values={buckets.map((bucket) =>
                bucket.startedAt > now ? null : bucket.inferenceRequests,
              )}
            />
          </div>
          <div className="usage-metric-footer usage-request-footer">
            <span className="usage-request-success">
              成功 <b>{summary ? number.format(summary.inferenceSucceeded) : '—'}</b>
            </span>
            <span
              className={
                summary?.inferenceFailed
                  ? 'usage-request-failed has-failures'
                  : 'usage-request-failed'
              }
            >
              失败 <b>{summary ? number.format(summary.inferenceFailed) : '—'}</b>
            </span>
            <span className="usage-response-latency" title="从进入调度到首个有效输出，包含排队时间">
              平均首响 <b>{formatDuration(summary?.averageFirstResponseMs)}</b>
            </span>
            <span
              className="usage-response-latency"
              title="从排队结束、开始执行到首个有效输出，不含排队时间"
            >
              平均模型首响 <b>{formatDuration(summary?.averageModelFirstResponseMs)}</b>
            </span>
          </div>
        </div>
        <div className="usage-metric-card trend-output">
          <div className="usage-metric-body">
            <div className="usage-metric-copy">
              <span className="usage-metric-label">
                <Coins size={17} aria-hidden="true" />总 Token
              </span>
              <strong>
                <TokenCount value={summary?.totalTokens} unknown={unavailable} />
              </strong>
            </div>
            <UsageSparkline
              label="总 Token"
              values={buckets.map((bucket) =>
                !hasTokenUsage || bucket.startedAt > now
                  ? null
                  : bucket.inferenceRequests
                    ? bucket.totalTokens
                    : 0,
              )}
            />
          </div>
          <div className="usage-metric-footer">
            <span>
              输入 Token{' '}
              <b>
                <TokenCount value={summary?.inputTokens} unknown={unavailable} />
              </b>
            </span>
            <span>
              输出 Token{' '}
              <b>
                <TokenCount value={summary?.outputTokens} unknown={unavailable} />
              </b>
            </span>
          </div>
        </div>
        <div className="usage-metric-card trend-cache">
          <div className="usage-metric-body">
            <div className="usage-metric-copy">
              <span className="usage-metric-label">
                <Gauge size={17} aria-hidden="true" />
                缓存命中率
              </span>
              <strong>{cacheRate(summary?.cacheHitRate, unavailable)}</strong>
            </div>
            <UsageSparkline
              label="缓存命中率"
              ratio
              values={buckets.map((bucket) =>
                bucket.startedAt > now ? null : bucket.cacheHitRate,
              )}
            />
          </div>
          <div className="usage-metric-footer">
            <span>
              缓存输入 Token{' '}
              <b>
                <TokenCount value={summary?.cachedInputTokens} unknown={unavailable} />
              </b>
            </span>
            <span>
              {summary
                ? `${number.format(summary.cacheKnownRequests)} / ${number.format(summary.inferenceRequests)} 次缓存用量已知`
                : '按输入 Token 加权'}
            </span>
          </div>
        </div>
      </div>
      <div className={`usage-coverage ${unknownRequests ? 'is-partial' : ''}`}>
        <span className="coverage-marker" aria-hidden="true" />
        <p>
          {!summary ? (
            loading ? (
              '正在读取用量…'
            ) : (
              '用量暂时无法读取，请刷新重试。'
            )
          ) : !summary.inferenceRequests ? (
            '当前范围没有模型请求。'
          ) : (
            <>
              已获取{' '}
              <strong>
                {number.format(summary.usageKnownRequests)} /{' '}
                {number.format(summary.inferenceRequests)}
              </strong>{' '}
              次模型请求的完整用量
              {unknownRequests
                ? `，${number.format(unknownRequests)} 次未知。Token 仅汇总已知数据。`
                : '，用量数据完整。'}
            </>
          )}
          {summary && summary.total > summary.inferenceRequests
            ? ` 另有 ${number.format(summary.total - summary.inferenceRequests)} 条管理与检查记录。`
            : ''}
        </p>
      </div>
    </section>
  );
}
