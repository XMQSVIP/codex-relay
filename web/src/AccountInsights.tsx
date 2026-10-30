import { useToast } from './Toast';
import { useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { Account, AccountMetrics, AccountPolicy } from './api';
import { dateBounds, localDate } from './dateRange';
import { healthReason, safeErrorCode } from './accountStatus';
import { copyText } from './clipboard';
import { AccountQuotaPanel } from './AccountQuota';
import { TokenCount } from './TokenCount';

export const timestamp = (value: number | null | undefined) => value == null ? '暂无记录' : new Date(value).toLocaleString('zh-CN');
export function useAccountMetrics(token: string, accountId: string, revision: number) {
  const [data, setData] = useState<AccountMetrics | null>(null);
  const [error, setError] = useState('');
  const [day, setDay] = useState(localDate);
  const previousKey = useRef('');
  const refreshMetrics = useRef<() => Promise<boolean>>(async () => false);
  useEffect(() => {
    const timer = setInterval(() => setDay(localDate()), 30_000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    let controller: AbortController | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const key = `${token}:${accountId}:${day}`;
    if (previousKey.current !== key) { setData(null); previousKey.current = key; }
    const load = async (): Promise<boolean> => {
      clearTimeout(timer);
      controller?.abort();
      const request = new AbortController();
      controller = request;
      setError('');
      const bounds = dateBounds(day, day)!;
      try {
        const value = await api<AccountMetrics>(token, `/accounts/metrics?from=${bounds.from}&to=${bounds.to}${accountId ? `&accountId=${encodeURIComponent(accountId)}` : ''}`, undefined, request.signal);
        if (request.signal.aborted) return false;
        setData(value); setError('');
        return true;
      } catch {
        if (!request.signal.aborted) setError('用量读取失败，显示最近成功读取的数据。');
        return false;
      } finally { if (!request.signal.aborted) timer = setTimeout(load, 30_000); }
    };
    refreshMetrics.current = load;
    void load();
    return () => { controller?.abort(); clearTimeout(timer); refreshMetrics.current = async () => false; };
  }, [token, accountId, revision, day]);
  return { data, error, day, reload: () => refreshMetrics.current() };
}

export function AccountInsights({ account, policy, token, refresh, retentionDays }: { account: Account; policy?: AccountPolicy; token: string; refresh: number; retentionDays: number }) {
  const { data, error, day } = useAccountMetrics(token, account.id, refresh);
  const metric = data?.accounts.find(row => row.id === account.id);
  const [now, setNow] = useState(Date.now);
  const notify = useToast();
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  const health = policy?.health;
  const remaining = Math.max(0, Math.ceil(((health?.retryAt ?? 0) - now) / 1000));
  const code = safeErrorCode(health?.reason);
  const link = `accountId=${account.id}&kind=inference&startDate=${day}&endDate=${day}`;
  return <section className="account-insights" aria-label={`${account.name}的详情`}>
    {account.source !== 'api-key' ? <AccountQuotaPanel id={account.id} token={token} quota={policy?.quota} configured={!!policy?.route} /> : null}
    <div className="account-insight-status"><h4>状态详情</h4>
      <p>{health?.state === 'cooling' ? remaining > 0 ? `${remaining} 秒后检查恢复` : '等待恢复检查' : health?.state === 'halfOpen' ? '等待下一次请求检查恢复' : '当前没有已知调度故障'}</p>
      {health?.reason ? <p>{healthReason(health.reason)}</p> : null}
      {code ? <button className="text-button" onClick={() => void copyText(code).then(() => notify('错误码已复制'), () => notify('复制失败，请手动选择错误码', 'error'))}>{code} · 复制</button> : null}
      <p>连续连接失败：{health?.consecutiveFailures ?? 0}</p><p>最近验证：{timestamp(health?.lastVerifiedAt)}</p>
      {health?.sharedUpstream ? <p>此限制影响共享上游连接。</p> : null}

    </div>
    <div><h4>今日中转站用量</h4>{error ? <p role="alert" className="field-error">{error}</p> : null}
      <dl className="account-token-grid">{([['输入 Token', <TokenCount value={metric?.inputTokens} />], ['输出 Token', <TokenCount value={metric?.outputTokens} />], ['缓存 Token', <TokenCount value={metric?.cachedInputTokens} />], ['推理请求', data ? String(metric?.requestCount ?? 0) : '—'], ['完成成功率', metric?.successRate == null ? '—' : `${(metric.successRate * 100).toFixed(1)}%`]] as const).map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
      <p className="helper">{metric?.usageKnownRequests ?? 0} 次用量已知；缓存已包含在输入中。记录保留 {retentionDays} 天。</p>
      <p className="helper">更新于 {timestamp(data?.updatedAt)}</p>
      <div className="account-insight-links"><a href={`?view=requests&${link}`}>查看请求记录</a><a href={`?view=statistics&${link}`}>更多统计</a></div>
    </div>
    <div className="account-model-metrics"><h4>按模型统计</h4><table><thead><tr><th>模型</th><th>请求</th><th>成功率</th><th>总 Token</th></tr></thead><tbody>{data?.models.map(row => <tr key={row.id ?? 'unknown'}><th>{row.id ?? '未知模型'}</th><td>{row.requestCount}</td><td>{row.successRate == null ? '—' : `${(row.successRate * 100).toFixed(1)}%`}</td><td><TokenCount value={row.totalTokens} /></td></tr>)}</tbody></table>{!data?.models.length ? <p className="helper">{data ? '今天暂无模型记录' : error ? '暂时无法读取' : '正在读取…'}</p> : null}</div>
  </section>;
}
