import { UsageRankings } from './UsageRankings';
import { accountGroupIds } from './accountMembership';
import { useToast } from './Toast';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { Activity, ArrowLeft, ArrowRight, ChevronDown, RefreshCw, Settings2 } from 'lucide-react';
import { api } from './api';
import type { Operations, RequestRecord, Statistics } from './api';
import { goTo, useQueryState, useQueryNumber } from './navigation';
import { dateBounds, localDate } from './dateRange';
import { ClearRequestRecords } from './ClearRequestRecords';
import { StorageStatus } from './RuntimeDetails';
import {
  RequestDetailsDialog,
  RequestRows,
  formatDuration,
  requestKinds,
  requestStatuses,
} from './RequestRecords';
import { UsageOverview } from './UsageOverview';
import { UsageTrend } from './UsageTrend';
import { UsageScheduling } from './UsageScheduling';
import { RequestFilters } from './RequestFilters';

const number = new Intl.NumberFormat('zh-CN');

export function StatisticsPage({
  token,
  operations: ops,
  refreshVersion,
  mode = 'summary',
}: {
  mode?: 'summary' | 'records';
  token: string;
  operations: Operations;
  refreshVersion: number;
}) {
  const [selected, setSelected] = useState<RequestRecord | null>(null);
  const detailTrigger = useRef<HTMLButtonElement | null>(null);
  const recordsRegion = useRef<HTMLDivElement | null>(null);
  const closeDetails = () => {
    setSelected(null);
    (detailTrigger.current?.isConnected ? detailTrigger.current : recordsRegion.current)?.focus();
  };
  const [recordsVersion, setRecordsVersion] = useState(0);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const filtersId = useId();
  const filtersToggle = useRef<HTMLButtonElement>(null);
  const notify = useToast();
  const statisticsRequest = useRef<AbortController | null>(null);
  const [keyName, setKeyName] = useQueryState('keyName', '');
  const [keyId, setKeyId] = useQueryState('keyId', '');
  const [model, setModel] = useQueryState('model', '');
  const [errorCode, setErrorCode] = useQueryState('errorCode', '');
  const [accountName, setAccountName] = useQueryState('accountName', '');
  const [groupName, setGroupName] = useQueryState('groupName', '');
  const extra = useMemo(
    () => ({ keyId, model, errorCode, keyName, accountName, groupName }),
    [keyId, model, errorCode, keyName, accountName, groupName],
  );
  const setExtra = (value: typeof extra) => {
    setKeyName(value.keyName);
    setKeyId(value.keyId);
    setModel(value.model);
    setErrorCode(value.errorCode);
    setAccountName(value.accountName);
    setGroupName(value.groupName);
  };
  const advancedForm = useRef<HTMLFormElement>(null);
  const today = localDate();
  const [startDate] = useQueryState('startDate', today);
  const [endDate] = useQueryState('endDate', today);
  const [dateError, setDateError] = useState('');
  const refreshSeconds = 30;
  const [manualRefresh, setManualRefresh] = useState(0);
  const [groupId, setGroup] = useQueryState('groupId', '');
  const [accountId, setAccount] = useQueryState('accountId', '');
  const [kind, setKind] = useQueryState('kind', '');
  const [status, setStatus] = useQueryState('status', '');
  const [page, setPage] = useQueryNumber('page', 1);
  const [requestedPageSize, setPageSize] = useQueryNumber('pageSize', 20);
  const pageSize = [20, 50, 100].includes(requestedPageSize) ? requestedPageSize : 20;
  const [data, setData] = useState<Statistics | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    const controller = new AbortController();
    statisticsRequest.current = controller;
    let timer: ReturnType<typeof setTimeout>;
    setData(null);
    setLoading(true);
    const bounds = dateBounds(startDate, endDate);
    if (!bounds) {
      setError('日期范围无效，请重新选择开始和结束日期。');
      setLoading(false);
      return () => controller.abort();
    }
    const params = new URLSearchParams({
      from: String(bounds.from),
      to: String(bounds.to),
      page: String(page),
      pageSize: String(pageSize),
    });
    for (const [key, value] of Object.entries({ groupId, accountId, kind, status, ...extra }))
      if (value) params.set(key, value);
    const load = async () => {
      try {
        const next = await api<Statistics>(
          token,
          `/statistics?${params}`,
          undefined,
          controller.signal,
        );
        if (!controller.signal.aborted) {
          setData(next);
          setError('');
          if (next.page !== page) {
            const url = new URLSearchParams(location.search);
            url.set('page', String(next.page));
            goTo(`?${url}`, true);
          }
        }
      } catch (e) {
        if (!controller.signal.aborted) setError(e instanceof Error ? e.message : '统计加载失败');
      } finally {
        if (!controller.signal.aborted) {
          setLoading(false);
          timer = setTimeout(load, refreshSeconds * 1000);
        }
      }
    };
    void load();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [
    token,
    startDate,
    endDate,
    refreshSeconds,
    manualRefresh,
    groupId,
    accountId,
    kind,
    status,
    page,
    pageSize,
    refreshVersion,
    extra,
    recordsVersion,
  ]);
  const applyDates = (start: string, end: string, reset = false) => {
    if (!dateBounds(start, end)) {
      setDateError('结束日期不能早于开始日期。');
      return;
    }
    const params = new URLSearchParams(location.search);
    params.delete('hours');
    params.delete('page');
    if (reset) {
      params.delete('startDate');
      params.delete('endDate');
    } else {
      params.set('startDate', start);
      params.set('endDate', end);
    }
    setDateError('');
    goTo(`?${params}`);
  };
  const filter = (setter: (v: string) => void, value: string) => {
    setter(value);
    setPage(1);
  };
  const hasFilters = !!(
    groupId ||
    accountId ||
    kind ||
    status ||
    Object.values(extra).some(Boolean)
  );
  const clearFilters = () => {
    setGroup('');
    setAccount('');
    setKind('');
    setStatus('');
    setExtra({ keyId: '', model: '', errorCode: '', keyName: '', accountName: '', groupName: '' });
    setPage(1);
    advancedForm.current?.reset();
  };
  const recordsCleared = (deleted: number) => {
    statisticsRequest.current?.abort();
    notify(
      deleted
        ? `已删除全部 ${number.format(deleted)} 条请求记录，历史用量统计已清空。`
        : '暂无请求记录可清空。',
    );
    setData(null);
    setPage(1);
    setRecordsVersion((value) => value + 1);
  };
  const s = data?.summary;
  const finished = (s?.succeeded ?? 0) + (s?.failed ?? 0);
  const presetStart = (days: number) => {
    const start = new Date(`${today}T00:00:00`);
    start.setDate(start.getDate() - days + 1);
    return localDate(start);
  };
  return (
    <div className="ops-layout statistics-layout">
      <header className="statistics-heading">
        <div className="statistics-update">
          <p>
            <span className="update-indicator" aria-hidden="true" />每 {refreshSeconds} 秒更新
            <span className="metadata-divider" />
            保留 {ops.retentionDays} 天
          </p>
          <button
            className="toolbar-icon statistics-refresh"
            type="button"
            disabled={loading}
            onClick={() => setManualRefresh((value) => value + 1)}
            aria-label={mode === 'summary' ? '刷新用量统计' : '刷新请求记录'}
            title={mode === 'summary' ? '刷新用量统计' : '刷新请求记录'}
          >
            <RefreshCw size={18} className={loading ? 'spin' : ''} aria-hidden="true" />
          </button>
        </div>
        <div className="statistics-heading-actions">
          {mode === 'summary' ? (
            <div className="usage-switch date-presets" role="group" aria-label="快捷日期">
              {([1, 7, 30] as const).map((days) => (
                <button
                  key={days}
                  type="button"
                  aria-pressed={startDate === presetStart(days) && endDate === today}
                  onClick={() => applyDates(presetStart(days), today, days === 1)}
                >
                  {days === 1 ? '今天' : `近 ${days} 天`}
                </button>
              ))}
            </div>
          ) : null}
          <form
            className="date-range"
            aria-label="筛选日期"
            key={`${startDate}:${endDate}`}
            onSubmit={(event) => {
              event.preventDefault();
              const values = new FormData(event.currentTarget);
              applyDates(String(values.get('startDate')), String(values.get('endDate')));
            }}
          >
            <label>
              <span className="visually-hidden">开始日期</span>
              <input type="date" name="startDate" defaultValue={startDate} max={today} required />
            </label>
            <label>
              <span className="visually-hidden">结束日期</span>
              <input type="date" name="endDate" defaultValue={endDate} max={today} required />
            </label>
            <button className="button secondary" type="submit">
              筛选日期
            </button>
            {mode === 'summary' ? (
              <button
                ref={filtersToggle}
                type="button"
                className={`button secondary statistics-filter-button ${hasFilters ? 'has-filters' : ''}`}
                aria-expanded={filtersOpen}
                aria-controls={filtersId}
                aria-label={
                  hasFilters
                    ? `筛选，${[groupId, accountId, kind, status, ...Object.values(extra)].filter(Boolean).length} 项已应用`
                    : '筛选'
                }
                onClick={() => setFiltersOpen((value) => !value)}
              >
                <Settings2 size={15} aria-hidden="true" />
                筛选
                {hasFilters ? (
                  <span className="count" aria-hidden="true">
                    {
                      [groupId, accountId, kind, status, ...Object.values(extra)].filter(Boolean)
                        .length
                    }
                  </span>
                ) : null}
              </button>
            ) : null}
            {mode === 'records' ? (
              <button
                className="text-button"
                type="button"
                onClick={(event) => {
                  event.currentTarget.form?.reset();
                  applyDates(today, today, true);
                }}
              >
                今天
              </button>
            ) : null}
          </form>
        </div>
      </header>
      {dateError || error ? (
        <div className="message error" role="alert">
          {dateError || error}
        </div>
      ) : null}

      {mode === 'summary' ? (
        <section
          className="panel statistics-filters"
          id={filtersId}
          aria-label="统计筛选"
          hidden={!filtersOpen}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.stopPropagation();
              setFiltersOpen(false);
              filtersToggle.current?.focus();
            }
          }}
        >
          <div className="section-heading">
            <div>
              <h2>筛选条件</h2>
              <p>同时应用到指标、趋势、排行与记录；实时容量保持全局值。</p>
            </div>
            {hasFilters ? (
              <button type="button" className="text-button" onClick={clearFilters}>
                清除筛选
              </button>
            ) : null}
          </div>
          <div className="record-filters">
            <Filter
              label="分组"
              value={groupId}
              set={(v) => {
                filter(setGroup, v);
                setGroupName('');
                setAccount('');
                setAccountName('');
              }}
              options={ops.groups.map((g) => [g.id, g.name])}
            />
            <Filter
              label="账户"
              value={accountId}
              set={(v) => {
                filter(setAccount, v);
                setAccountName('');
              }}
              options={ops.accounts
                .filter((a) => !groupId || accountGroupIds(a).includes(groupId))
                .map((a) => [a.id, a.name])}
            />
            <Filter
              label="请求类型"
              value={kind}
              set={(v) => filter(setKind, v)}
              options={Object.entries(requestKinds)}
            />
            <Filter
              label="请求状态"
              value={status}
              set={(v) => filter(setStatus, v)}
              options={Object.entries(requestStatuses)}
            />
          </div>
          <details className="record-advanced">
            <summary>
              <Settings2 size={14} aria-hidden="true" />
              更多筛选
              {Object.values(extra).some(Boolean) ? (
                <span className="count">
                  {Object.values(extra).filter(Boolean).length} 项已应用
                </span>
              ) : (
                <span>名称关键词、Key ID、模型、错误码</span>
              )}
              <ChevronDown size={14} aria-hidden="true" />
            </summary>
            <form
              key={`${keyId}:${model}:${errorCode}:${accountName}:${groupName}`}
              ref={advancedForm}
              className="record-filters"
              onSubmit={(e) => {
                e.preventDefault();
                const values = new FormData(e.currentTarget);
                setExtra({
                  keyName,
                  keyId: String(values.get('keyId') ?? '').trim(),
                  model: String(values.get('model') ?? '').trim(),
                  errorCode: String(values.get('errorCode') ?? '').trim(),
                  accountName: String(values.get('accountName') ?? '').trim(),
                  groupName: String(values.get('groupName') ?? '').trim(),
                });
                setPage(1);
              }}
            >
              <label>
                账户名称关键词
                <input defaultValue={accountName} name="accountName" maxLength={128} />
              </label>
              <label>
                分组名称关键词
                <input defaultValue={groupName} name="groupName" maxLength={128} />
              </label>
              <label>
                Key ID
                <input
                  defaultValue={keyId}
                  name="keyId"
                  maxLength={36}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="完整 Key ID"
                />
              </label>
              <label>
                模型
                <input
                  defaultValue={model}
                  name="model"
                  maxLength={128}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="完整模型 ID"
                />
              </label>
              <label>
                错误码
                <input
                  defaultValue={errorCode}
                  name="errorCode"
                  maxLength={128}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="例如 RPC_CLOSED"
                />
              </label>
              <button className="button secondary" type="submit">
                应用筛选
              </button>
            </form>
          </details>
        </section>
      ) : null}
      {mode === 'summary' ? (
        <>
          <UsageOverview data={data} loading={loading} />
          <div className="usage-visuals-grid">
            <UsageScheduling operations={ops} />
            <UsageTrend
              key={`${startDate}:${endDate}`}
              data={data}
              loading={loading}
              error={!!error}
            />
          </div>
          <div className="usage-details-grid">
            <section className="panel usage-quality" aria-label="运行质量与排队">
              <div className="section-heading usage-quality-heading">
                <div>
                  <h2>运行质量与排队</h2>
                  <p>
                    {s
                      ? `${number.format(s.failed)} 次失败，${number.format(s.rejected)} 次拒绝`
                      : '请求状态与耗时'}{' '}
                  </p>
                </div>
              </div>
              <div className="usage-quality-note">
                状态统计包含当前筛选范围的全部记录；首响与总耗时仅统计模型请求的已知样本。实时容量保持全局值。
              </div>
              <div className="metric-strip">
                <Metric
                  label="完成成功率"
                  value={finished ? `${Math.round(((s?.succeeded ?? 0) / finished) * 100)}%` : '—'}
                  sub={`${s?.succeeded ?? 0} 次成功 / ${finished} 次完成；不含拒绝与取消`}
                />
                <Metric
                  label="平均首响"
                  value={formatDuration(s?.averageFirstResponseMs)}
                  sub="从进入调度到首个有效输出，含排队"
                />
                <Metric
                  label="平均总耗时"
                  value={formatDuration(s?.averageTotalMs)}
                  sub="含排队、执行与清理"
                />
                <Metric
                  label="取消或中断"
                  value={s ? number.format(s.cancelled) : '—'}
                  sub="不计入完成成功率"
                />
              </div>
              <div className="queue-metrics">
                <Metric
                  label="实时排队"
                  value={`${ops.queued} / ${ops.queueLimit}`}
                  sub={`全局并发 ${ops.active} / ${ops.globalLimit}，不受筛选影响`}
                />
                <Metric
                  label="等待超时"
                  value={s ? number.format(s.timedOut) : '—'}
                  sub="按当前筛选条件"
                />
                <Metric
                  label="队列满拒绝"
                  value={s ? number.format(s.queueFull) : '—'}
                  sub="包含在拒绝请求总数中"
                />
                <Metric
                  label="平均排队"
                  value={formatDuration(s?.averageWaitMs)}
                  sub="包含无需等待的请求，不含仍在排队的请求"
                />
              </div>
            </section>
            <UsageRankings
              accounts={data?.accountUsage ?? []}
              keys={data?.keyUsage ?? []}
              models={data?.modelUsage ?? []}
              loading={loading}
              error={!!error}
            />
          </div>
          <a
            className="button secondary statistics-records-link"
            href={`?${(() => {
              const params = new URLSearchParams(location.search);
              params.set('view', 'requests');
              params.delete('page');
              return params;
            })()}`}
          >
            查看对应请求记录
            <ArrowRight size={15} aria-hidden="true" />
          </a>
        </>
      ) : null}
      {mode === 'records' ? (
        <section className="panel records-panel">
          <div className="section-heading records-heading">
            <h2
              className="records-title"
              tabIndex={0}
              title="首响与总耗时包含排队时间，模型首响不含排队；点击详情查看完整记录。"
            >
              请求记录 <span className="count">{s?.total ?? 0} 条</span>
            </h2>
            <RequestFilters
              fields={[
                { key: 'keyName', label: '密钥名称', value: keyName },
                { key: 'keyId', label: 'Key ID', value: keyId, maxLength: 36, hidden: true },
                {
                  key: 'groupId',
                  label: '分组',
                  value: groupId,
                  nameSearch: { key: 'groupName', value: groupName },
                  options: ops.groups.map((g) => [g.id, g.name]),
                },
                {
                  key: 'accountId',
                  label: '账户',
                  value: accountId,
                  nameSearch: { key: 'accountName', value: accountName },
                  options: ops.accounts
                    .filter((a) => !groupId || accountGroupIds(a).includes(groupId))
                    .map((a) => [a.id, a.name]),
                },
                {
                  key: 'kind',
                  label: '请求类型',
                  value: kind,
                  hidden: true,
                  options: Object.entries(requestKinds),
                },
                {
                  key: 'status',
                  label: '请求状态',
                  value: status,
                  options: Object.entries(requestStatuses),
                },
                { key: 'model', label: '模型', value: model },
                { key: 'errorCode', label: '错误码', value: errorCode, hidden: true },
              ]}
              apply={(key, value) => {
                const params = new URLSearchParams(location.search);
                if (key === 'accountId' || key === 'accountName') {
                  params.delete('accountId');
                  params.delete('accountName');
                }
                if (key === 'groupId' || key === 'groupName') {
                  params.delete('groupId');
                  params.delete('groupName');
                  params.delete('accountId');
                  params.delete('accountName');
                }
                if (value) params.set(key, value);
                else params.delete(key);
                params.delete('page');
                goTo(`?${params}`);
              }}
              clear={clearFilters}
            />

            <div className="records-heading-actions">
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
                  第 {data?.page ?? page} / {Math.max(1, Math.ceil((s?.total ?? 0) / pageSize))} 页
                  · 共 {s?.total ?? 0} 条
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
                  disabled={loading || !!error || page * pageSize >= (s?.total ?? 0)}
                  onClick={() => setPage((p) => p + 1)}
                >
                  <ArrowRight size={15} aria-hidden="true" />
                </button>
              </nav>
              <ClearRequestRecords
                token={token}
                active={ops.active}
                queued={ops.queued}
                disabled={ops.draining}
                onCleared={recordsCleared}
              />
            </div>
          </div>
          <div
            ref={recordsRegion}
            className={`records-scroll${data?.records.length ? '' : ' is-empty'}`}
            tabIndex={0}
            role="region"
            aria-label="请求记录，可横向滚动"
            aria-busy={loading}
          >
            <table className="records-table">
              <caption className="visually-hidden">
                请求记录，包含密钥名称、账户、模型、推理强度、端点、上游、接入、UA
                标识、Token、首响、模型首响、总耗时、状态与时间
              </caption>
              <thead>
                <tr>
                  <th scope="col">密钥名称</th>
                  <th scope="col">账户 / 分组</th>
                  <th scope="col">模型</th>
                  <th scope="col">推理强度</th>
                  <th scope="col">连接信息</th>
                  <th scope="col">Token</th>
                  <th scope="col">延迟</th>
                  <th scope="col">状态</th>
                  <th scope="col">请求时间</th>
                  <th scope="col">
                    <span className="visually-hidden">详情</span>
                  </th>
                </tr>
              </thead>
              <RequestRows
                records={data?.records ?? []}
                onOpen={(record, trigger) => {
                  detailTrigger.current = trigger;
                  setSelected(record);
                }}
              />
            </table>
          </div>
          {!data?.records.length ? (
            <div className="compact-empty records-empty">
              <Activity size={28} aria-hidden="true" />
              <h3>
                {loading ? '正在读取请求记录' : error ? '暂时无法读取记录' : '当前范围没有请求记录'}
              </h3>
              <p>
                {loading
                  ? '正在同步所选时间范围的数据…'
                  : error
                    ? '请使用右上角刷新按钮重试。'
                    : s?.total
                      ? '这一页没有记录，请返回上一页。'
                      : hasFilters
                        ? '调整时间范围或清除筛选条件后重试。'
                        : '客户端发起请求后，这里会显示用量、耗时与执行结果。'}
              </p>
              {!loading && !error ? (
                hasFilters ? (
                  <button className="button secondary" onClick={clearFilters}>
                    清除筛选
                  </button>
                ) : (
                  <a className="button secondary" href="?view=keys">
                    查看客户端接入
                    <ArrowRight size={15} aria-hidden="true" />
                  </a>
                )
              ) : null}
            </div>
          ) : null}
        </section>
      ) : null}
      <div className="statistics-storage-footer">
        <StorageStatus operations={ops} />
      </div>
      {selected ? (
        <RequestDetailsDialog
          token={token}
          record={data?.records.find((record) => record.id === selected.id) ?? selected}
          close={closeDetails}
        />
      ) : null}
    </div>
  );
}
function Metric({ label, value, sub }: { label: string; value: React.ReactNode; sub: string }) {
  return (
    <div className="metric" title={sub}>
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{sub}</small>
    </div>
  );
}
function Filter({
  label,
  value,
  options,
  set,
}: {
  label: string;
  value: string;
  options: string[][];
  set: (v: string) => void;
}) {
  const id = useId();
  return (
    <div>
      <label htmlFor={id}>{label}</label>
      <select id={id} value={value} onChange={(e) => set(e.target.value)}>
        <option value="">全部{label}</option>
        {options.map(([v, text]) => (
          <option value={v} key={v}>
            {text}
          </option>
        ))}
      </select>
    </div>
  );
}
