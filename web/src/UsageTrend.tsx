import { useId, useRef, useState } from 'react';
import type { Statistics } from './api';
import { exactTokens, formatTokens } from './tokenFormat';

type Bucket = Statistics['trend'][number];
type Point = { x: number; y: number };
const number = new Intl.NumberFormat('zh-CN');
const percent = new Intl.NumberFormat('zh-CN', { style: 'percent', maximumFractionDigits: 1 });
const time = (value: number) =>
  new Date(value).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
export const cacheRate = (value: number | null | undefined, unknown = '未知') =>
  value == null ? unknown : percent.format(value);

// A missing sample starts a new segment; it must never become a zero or a bridge.
function linePath(points: (Point | null)[]) {
  let connected = false;
  return points
    .map((point) => {
      if (!point) {
        connected = false;
        return '';
      }
      const command = connected ? 'L' : 'M';
      connected = true;
      return `${command}${point.x.toFixed(2)},${point.y.toFixed(2)}`;
    })
    .join(' ');
}

export function UsageSparkline({
  values,
  label,
  ratio = false,
}: {
  values: (number | null)[];
  label: string;
  ratio?: boolean;
}) {
  const max = ratio ? 1 : Math.max(1, ...values.map((value) => value ?? 0));
  const points = values.map((value, index) =>
    value == null
      ? null
      : {
          x: 5 + (index / Math.max(1, values.length - 1)) * 142,
          y: 59 - Math.min(1, value / max) * 50,
        },
  );
  const known = points.filter((point): point is Point => point !== null);
  return (
    <svg
      className="usage-sparkline"
      viewBox="0 0 152 68"
      role="img"
      aria-label={`${label}趋势${known.length ? '' : '，暂无已知数据'}`}
    >
      <path className="sparkline-guide" d="M5,59 H147" />
      <path className="sparkline-line" d={linePath(points)} />
      {known.map((point, index) => (
        <circle key={index} cx={point.x} cy={point.y} r={known.length === 1 ? 3 : 1.7} />
      ))}
    </svg>
  );
}

const seriesFields = [
  { field: 'inputTokens', label: '输入（含缓存）', name: '输入', style: 'input' },
  { field: 'outputTokens', label: '输出', name: '输出', style: 'output' },
  { field: 'cachedInputTokens', label: '缓存输入', name: '缓存', style: 'cache' },
] as const;

export function UsageTrend({
  data,
  loading,
  error,
}: {
  data: Statistics | null;
  loading: boolean;
  error: boolean;
}) {
  const headingId = useId();
  const [activeBucket, setActiveBucket] = useState<number | null>(null);
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const buckets = data?.trend ?? [];
  const now = Date.now();
  const series = seriesFields.map((series) => {
    const known = buckets.some(
      (bucket) =>
        bucket.startedAt <= now && bucket.inferenceRequests > 0 && bucket[series.field] !== null,
    );
    return {
      ...series,
      values: buckets.map((bucket) =>
        !known || bucket.startedAt > now
          ? null
          : bucket.inferenceRequests
            ? bucket[series.field]
            : 0,
      ),
    };
  });
  const max = Math.max(1, ...series.flatMap((series) => series.values.map((value) => value ?? 0)));
  const points = series.map((series) =>
    series.values.map((value, index) =>
      value == null
        ? null
        : {
            x: ((index + 0.5) / buckets.length) * 960,
            y: 184 - (value / max) * 170,
          },
    ),
  );
  const latestBucket = buckets.findLastIndex(
    (bucket) => bucket.inferenceRequests > 0 && bucket.startedAt <= now,
  );
  const selectedIndex = activeBucket ?? (latestBucket < 0 ? null : latestBucket);
  const selected = selectedIndex === null ? undefined : buckets[selectedIndex];
  const hasRequests = buckets.some((bucket) => bucket.inferenceRequests > 0);
  const hasUsage = series.some((series) => series.values.some((value) => value !== null));
  const tokenValue = (bucket: Bucket, field: (typeof seriesFields)[number]['field']) =>
    exactTokens(bucket[field], bucket.inferenceRequests ? '未知' : '—');
  const interval = (index: number) =>
    `${time(buckets[index].startedAt)} 至 ${time(Math.min(data!.to, (buckets[index + 1]?.startedAt ?? data!.to + 1) - 1))}`;
  return (
    <section className="panel usage-trend" aria-labelledby={headingId} aria-busy={loading}>
      <div className="section-heading usage-trend-heading">
        <div>
          <h2 id={headingId}>用量趋势</h2>
          <p>缓存已包含在输入中，各曲线独立展示</p>
        </div>
        <div className="usage-trend-legend" aria-label="趋势图例">
          {seriesFields.map((series) => (
            <span key={series.field} className={`trend-${series.style}`}>
              <i aria-hidden="true" />
              {series.label}
            </span>
          ))}
        </div>
      </div>
      <div
        className="usage-trend-plot"
        role="group"
        aria-label="用量趋势，输入、输出和缓存 Token。使用左右方向键查看各时段。"
      >
        <div className="usage-trend-axis" aria-hidden="true">
          <span>{formatTokens(max)}</span>
          <span>{formatTokens(max / 2)}</span>
          <span>0</span>
        </div>
        <div className="usage-trend-canvas">
          <svg viewBox="0 0 960 200" preserveAspectRatio="none" aria-hidden="true">
            {[14, 99, 184].map((y) => (
              <line className="usage-grid-line" key={y} x1={0} x2={960} y1={y} y2={y} />
            ))}
            {series.map((series, index) => (
              <g key={series.field} className={`trend-${series.style}`}>
                <path className="usage-series-line" d={linePath(points[index])} />
                {points[index].map((point, i) =>
                  point && buckets[i].inferenceRequests > 0 ? (
                    <circle key={i} cx={point.x} cy={point.y} r={3} />
                  ) : null,
                )}
              </g>
            ))}
            {selectedIndex !== null ? (
              <line
                className="usage-selection-line"
                x1={((selectedIndex + 0.5) / buckets.length) * 960}
                x2={((selectedIndex + 0.5) / buckets.length) * 960}
                y1={4}
                y2={190}
              />
            ) : null}
          </svg>
          <div
            className="usage-trend-targets"
            style={{
              gridTemplateColumns: `repeat(${Math.max(1, buckets.length)}, minmax(0, 1fr))`,
            }}
          >
            {buckets.map((bucket, index) => {
              const unknown =
                bucket.inferenceRequests > 0 &&
                seriesFields.some((series) => bucket[series.field] === null);
              return (
                <button
                  key={bucket.startedAt}
                  type="button"
                  ref={(element) => {
                    buttons.current[index] = element;
                  }}
                  className={`${selectedIndex === index ? 'is-active' : ''} ${unknown ? 'is-unknown' : ''}`}
                  tabIndex={index === (selectedIndex ?? 0) ? 0 : -1}
                  aria-label={`${interval(index)}，${bucket.inferenceRequests} 次模型请求，${bucket.totalTokens === null ? (bucket.inferenceRequests ? 'Token 未知' : '无模型请求') : `${number.format(bucket.totalTokens)} Token`}，输入 ${tokenValue(bucket, 'inputTokens')}，输出 ${tokenValue(bucket, 'outputTokens')}，缓存 ${tokenValue(bucket, 'cachedInputTokens')}，缓存命中率 ${cacheRate(bucket.cacheHitRate)}，${bucket.usageKnownRequests} 次用量已知`}
                  onPointerEnter={() => setActiveBucket(index)}
                  onFocus={() => setActiveBucket(index)}
                  onClick={() => setActiveBucket(index)}
                  onKeyDown={(event) => {
                    const next =
                      event.key === 'ArrowRight'
                        ? Math.min(buckets.length - 1, index + 1)
                        : event.key === 'ArrowLeft'
                          ? Math.max(0, index - 1)
                          : event.key === 'Home'
                            ? 0
                            : event.key === 'End'
                              ? buckets.length - 1
                              : null;
                    if (next !== null) {
                      event.preventDefault();
                      buttons.current[next]?.focus();
                    }
                  }}
                >
                  {unknown ? <span className="usage-unknown-marker" aria-hidden="true" /> : null}
                </button>
              );
            })}
          </div>
          {!hasRequests || !hasUsage ? (
            <p className="usage-trend-empty">
              {loading
                ? '正在读取用量趋势…'
                : error
                  ? '用量趋势暂时无法读取，请刷新重试。'
                  : hasRequests
                    ? '当前范围尚无已知 Token 用量'
                    : '当前范围暂无模型请求'}
            </p>
          ) : null}
        </div>
      </div>
      <div className="usage-trend-dates" aria-hidden="true">
        <span>{data ? time(data.from) : '—'}</span>
        <span>{data ? time(data.to) : '—'}</span>
      </div>
      <div className="usage-trend-readout" role="status">
        {selected ? (
          <>
            <div className="usage-readout-heading">
              <strong>{interval(selectedIndex!)}</strong>
              <span>{selected.inferenceRequests} 次模型请求</span>
            </div>
            <dl>
              {seriesFields.map((series) => (
                <div key={series.field} className={`trend-${series.style}`}>
                  <dt>{series.label}</dt>
                  <dd>{tokenValue(selected, series.field)}</dd>
                </div>
              ))}
              <div>
                <dt>缓存命中率</dt>
                <dd>
                  {cacheRate(selected.cacheHitRate, selected.inferenceRequests ? '未知' : '—')}
                </dd>
              </div>
            </dl>
            <p>
              {selected.usageKnownRequests} / {selected.inferenceRequests} 次完整用量已知；
              {selected.cacheKnownRequests} / {selected.inferenceRequests} 次缓存用量已知
            </p>
          </>
        ) : (
          <p>悬停、点击或用方向键查看时段详情</p>
        )}
      </div>
      <details className="usage-trend-table">
        <summary>查看时段数据</summary>
        <div
          className="usage-trend-table-scroll"
          tabIndex={0}
          role="region"
          aria-label="时段用量数据，可横向滚动"
        >
          <table>
            <thead>
              <tr>
                <th scope="col">时段</th>
                <th scope="col">请求数</th>
                {seriesFields.map((series) => (
                  <th key={series.field} scope="col">
                    {series.label}
                  </th>
                ))}
                <th scope="col">缓存命中率</th>
              </tr>
            </thead>
            <tbody>
              {buckets.map((bucket, index) => (
                <tr key={bucket.startedAt}>
                  <th scope="row">{interval(index)}</th>
                  <td>{bucket.inferenceRequests}</td>
                  {seriesFields.map((series) => (
                    <td key={series.field}>{tokenValue(bucket, series.field)}</td>
                  ))}
                  <td>{cacheRate(bucket.cacheHitRate, bucket.inferenceRequests ? '未知' : '—')}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </details>
    </section>
  );
}
