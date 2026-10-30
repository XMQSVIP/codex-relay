import { useToast } from './Toast';
import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { api, ApiError } from './api';
import type { AccountQuota as Quota } from './api';
import { timestamp } from './AccountInsights';
import { Modal } from './ui';
import { exactTokens, formatTokens } from './tokenFormat';

function quotaDuration(minutes?: number | null): string | undefined {
  if (minutes == null) return undefined;
  if (minutes === 10080) return '周';
  if (minutes % 1440 === 0) return `${minutes / 1440} 天`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时`;
  return `${minutes} 分钟`;
}

type QuotaWindow = Quota['windows'][number];
function quotaTone(window?: QuotaWindow, now = Date.now()) {
  if (window?.usedPercent == null || window.resetsAt == null || window.resetsAt <= now) return 'neutral';
  if (window.usedPercent >= 100) return 'error';
  return window.usedPercent > 90 ? 'warning' : 'success';
}
export function quotaLabel(quota?: Quota) {
  if (!quota || quota.status === 'unknown') return '额度未知';
  if (quota.status === 'unsupported') return '额度不支持';
  if (quota.blocked) return '额度受限';
  if (quota.status === 'error') return '额度读取失败';
  const window = quota.windows.find(w => w.bucket === 'codex' && w.usedPercent != null);
  return window ? `剩余 ${Math.max(0, 100 - window.usedPercent!).toFixed(1)}%${quota.stale ? ' · 已过期' : ''}` : '额度未知';
}
function quotaFailure(quota?: Quota): string | null {
  if (quota?.status !== 'error' && quota?.status !== 'unsupported') return null;
  const reasons: Record<string, string> = {
    QUOTA_TIMEOUT: '额度读取超时，请检查账户代理和上游网络连接。',
    RPC_TIMEOUT: '原生后端响应超时，请检查账户代理、上游网络连接和服务端进程状态。',
    QUOTA_UNSUPPORTED: '当前提供方或原生版本未提供可识别的额度数据。',
    OAUTH_NOT_SIGNED_IN: '账户尚未登录，请重新登录或导入凭据。',
    OAUTH_TOKEN_EXPIRED: '账户令牌已过期，请重新登录或导入有效凭据。',
    ROUTE_CHANGED: '账户身份或配置已变化，请重新启用账户后读取额度。',
    RPC_REMOTE: '原生后端拒绝了额度读取流程中的请求，请检查账户登录状态和服务端诊断。',
    RPC_CLOSED: '原生后端连接已断开，请检查服务端进程状态。',
  };
  const code = quota.errorCode;
  const message = code && reasons[code] || (quota.status === 'unsupported' ? '当前账户不支持读取额度。' : '额度读取失败，请检查服务端诊断。');
  return code ? `${message}（${code}）` : message;
}
export function creditBalance(quota?: Quota) {
  const credits = quota?.credits;
  return credits?.unlimited ? '无限' : credits?.balance != null ? `${credits.balance} 点` : credits?.hasCredits === false ? '暂无可用点数' : credits?.hasCredits ? '有点数，余额未知' : '未知';
}
function benefitStale(checkedAt?: number) { return checkedAt != null && Date.now() - checkedAt > 300_000; }
type QuotaProps = { id: string; token: string; quota?: Quota; configured: boolean };
function QuotaRefreshFailure({ quota, details }: { quota: Quota; details?: QuotaProps }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const cached = quota.windows.length > 0 || quota.credits != null || quota.resetCredits != null;
  return <div className="quota-refresh-failure">
    {details ? <button type="button" className="text-button quota-failure-trigger" onClick={() => setOpen(true)}>刷新失败 · 查看原因</button> : <small className="quota-stale">{quotaFailure(quota)}</small>}
    {cached ? <small className="helper">显示上次数据</small> : null}
    {open && details ? <Modal title="额度刷新详情" trapFocus busy={busy} close={() => setOpen(false)}><AccountQuotaPanel {...details} quota={quota} onBusyChange={setBusy} /></Modal> : null}
  </div>;
}
function ResetCardsButton({ quota, ...props }: QuotaProps) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  return <><button className="text-button quota-reset-trigger" onClick={() => setOpen(true)}>重置卡：{quota?.resetCredits == null ? '未知' : `${quota.resetCredits.availableCount} 张`}</button>{open ? <Modal title="重置卡" trapFocus busy={busy} close={() => setOpen(false)}><AccountQuotaPanel {...props} quota={quota} resetControls onBusyChange={setBusy} /></Modal> : null}</>;
}
function QuotaBenefits({ quota, reset }: { quota?: Quota; reset?: QuotaProps }) {
  if (quota?.status === 'unsupported') return null;
  return <div className="quota-benefits"><span>点数：{creditBalance(quota)}</span>{reset ? <ResetCardsButton {...reset} quota={quota} /> : <span>重置卡：{quota?.resetCredits == null ? '未知' : `${quota.resetCredits.availableCount} 张`}</span>}</div>;
}
type ResetAttempt = { context: string; idempotencyKey: string; creditId?: string | null };
type ResetAutomation = { enabled: boolean; revision: number; pendingRequest?: ResetAttempt | null; lastResult?: string | null };
function savedAttempt(id: string): ResetAttempt | null {
  try { const value = JSON.parse(sessionStorage.getItem(`quota-reset:${id}`) ?? 'null'); return value && typeof value.context === 'string' && typeof value.idempotencyKey === 'string' ? value : null; } catch { return null; }
}
export function AccountQuotaCell({ quota, apiAccount = false, ...reset }: QuotaProps & { apiAccount?: boolean }) {
  const available = quota?.status === 'supported' && quota.windows.some(w =>
    (w.bucket === 'codex' || !w.bucket) && w.usedPercent != null && w.resetsAt != null && w.resetsAt > Date.now());
  if (apiAccount && !available) return <span className="helper">不支持</span>;
  return <QuotaUsageCell quota={quota} reset={apiAccount ? undefined : { ...reset, quota }} />;
}
function QuotaUsageCell({ quota, reset }: { quota?: Quota; reset?: QuotaProps }) {
  const windows = quota?.windows.filter(w => w.bucket === 'codex' || !w.bucket) ?? [];
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const tooltipId = useId();
  const show = () => { clearTimeout(closeTimer.current); setOpen(true); };
  const leave = () => { closeTimer.current = setTimeout(() => setOpen(false), 150); };
  useEffect(() => () => clearTimeout(closeTimer.current), []);
  useLayoutEffect(() => {
    if (!open || !trigger.current || !popup.current) return;
    const position = () => {
      if (!trigger.current || !popup.current) return;
      const anchor = trigger.current.getBoundingClientRect();
      if (anchor.bottom <= 0 || anchor.top >= window.innerHeight) { setOpen(false); return; }
      const panel = popup.current.getBoundingClientRect();
      setPosition({
        left: Math.max(12, Math.min(anchor.left, window.innerWidth - panel.width - 12)),
        top: Math.max(12, Math.min(anchor.bottom + 8 + panel.height <= window.innerHeight - 12 ? anchor.bottom + 8 : anchor.top - panel.height - 8, window.innerHeight - panel.height - 12)),
      });
    };
    position();
    const dismiss = () => setOpen(false);
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') dismiss(); };
    const outside = (event: Event) => { if (event.target instanceof Node && !trigger.current?.contains(event.target) && !popup.current?.contains(event.target)) dismiss(); };
    const scroll = (event: Event) => { if (!(event.target instanceof Node) || !popup.current?.contains(event.target)) position(); };
    document.addEventListener('keydown', escape);
    document.addEventListener('pointerdown', outside);
    document.addEventListener('focusin', outside);
    window.addEventListener('scroll', scroll, true);
    window.addEventListener('resize', dismiss);
    return () => {
      document.removeEventListener('keydown', escape);
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('focusin', outside);
      window.removeEventListener('scroll', scroll, true);
      window.removeEventListener('resize', dismiss);
    };
  }, [open]);
  const now = Date.now();
  const current = (w: Quota['windows'][number]) => w.resetsAt != null && w.resetsAt > now;
  const primary = windows.filter(current).reduce<Quota['windows'][number] | undefined>((longest, w) => !longest || (w.durationMins ?? 0) > (longest.durationMins ?? 0) ? w : longest, undefined);
  const usage = primary?.usage;
  const partial = usage != null && usage.usageKnownRequests < usage.requestCount;
  const percent = (w?: Quota['windows'][number]) => w?.usedPercent == null ? '未知' : `${w.usedPercent.toFixed(1)}%`;
  const duration = (w: Quota['windows'][number]) => w.durationMins == null ? w.window === 'primary' ? '主要窗口' : '次要窗口' : `${quotaDuration(w.durationMins)}额度`;
  const tone = (w?: QuotaWindow) => quotaTone(w, now);
  // Removing a tooltip under a stationary pointer must not open the next row's tooltip.
  return <div className="quota-summary">
    <button ref={trigger} type="button" className="quota-summary-trigger" aria-label="查看当前周期用量详情" aria-describedby={open ? tooltipId : undefined} aria-expanded={open}
      onPointerMove={event => { if (event.pointerType === 'mouse') show(); }} onPointerLeave={leave} onFocus={show} onBlur={() => setOpen(false)} onClick={show}>
      <span className="quota-summary-total"><strong>{formatTokens(usage?.totalTokens)}</strong><span>Token{partial && usage?.totalTokens != null ? '（部分）' : ''}</span></span>
      <span className="quota-summary-meta"><span>{primary ? duration(primary) : '周期用量未知'}</span><span className={`quota-rate ${tone(primary)}`}>使用率 {percent(primary)}</span></span>
      <span className="quota-summary-bars" aria-hidden="true">{windows.map(w => <span key={`${w.bucket}:${w.window}`} className={`quota-mini-track ${tone(w)}`}><span style={{ width: `${current(w) ? Math.max(0, Math.min(100, w.usedPercent ?? 0)) : 0}%` }} /></span>)}</span>
    </button>
    <QuotaBenefits quota={quota} reset={reset} />
    {quota?.blocked ? <small className="quota-stale">额度受限</small> : null}
    {quota?.status === 'error' ? <QuotaRefreshFailure quota={quota} details={reset} /> : null}
    {!windows.length && quota?.status !== 'error' ? <small className="helper">{quotaLabel(quota)}</small> : null}
    {open ? createPortal(<div ref={popup} id={tooltipId} role="tooltip" className="quota-tooltip" style={position} onPointerEnter={() => clearTimeout(closeTimer.current)} onPointerLeave={leave}>
      <div className="quota-tooltip-heading">通用额度</div>
      {quotaFailure(quota) ? <p className="quota-stale">{quotaFailure(quota)}</p> : null}
      {quota?.status === 'error' && windows.length ? <p className="quota-stale">显示上次成功读取的数据{quota.checkedAt ? `（${timestamp(quota.checkedAt)}）` : ''}。</p> : null}
      {windows.map(w => {
        const data = current(w) ? w.usage : null;
        return <section className={`quota-tooltip-window ${tone(w)}`} key={`${w.bucket}:${w.window}`}>
          <div className="quota-tooltip-window-heading"><strong>{duration(w)}</strong><span className={`quota-rate ${tone(w)}`}>{percent(w)}</span></div>
          {w.usedPercent != null ? <progress max={100} value={Math.max(0, Math.min(100, w.usedPercent))} aria-label={`${duration(w)}使用率`} /> : null}
          <dl><div><dt>窗口消耗</dt><dd title={data?.totalTokens != null ? `${exactTokens(data.totalTokens)} Token` : undefined}>{formatTokens(data?.totalTokens)}{data?.totalTokens != null ? ' Token' : ''}</dd></div><div><dt>重置时间</dt><dd>{timestamp(w.resetsAt)}</dd></div></dl>
          {!current(w) && w.resetsAt != null ? <p className="quota-stale">窗口已结束，等待刷新</p> : null}
          {data && data.usageKnownRequests < data.requestCount ? <p className="quota-stale">{data.requestCount - data.usageKnownRequests} 个请求尚无完整 Token 用量</p> : null}
        </section>;
      })}
      {!windows.length ? <p className="quota-tooltip-empty">{quotaLabel(quota)}</p> : null}
      <p className="quota-tooltip-note">Token 为本站保留记录中的输入与输出总量，缓存不重复计入；使用率来自上游。主数字取最长有效窗口，各窗口不相加。</p>
    </div>, document.body) : null}
  </div>;
}
export function AccountQuotaPanel({ id, token, quota, configured, resetControls = false, onBusyChange }: QuotaProps & { resetControls?: boolean; onBusyChange?: (busy: boolean) => void }) {
  const [latest, setLatest] = useState<Quota>();
  const [pending, setPending] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const value = latest && (latest.lastAttemptAt ?? latest.checkedAt ?? 0) >= (quota?.lastAttemptAt ?? quota?.checkedAt ?? 0) ? latest : quota;
  const now = Date.now();
  const remainingWindow = value?.windows.find(w => w.bucket === 'codex' && w.usedPercent != null);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetPending, setResetPending] = useState(false);
  const [attempt, setAttempt] = useState<ResetAttempt | null>(() => savedAttempt(id));
  const [automation, setAutomation] = useState<ResetAutomation>();
  const [automationBusy, setAutomationBusy] = useState(false);
  const [automationError, setAutomationError] = useState(false);
  useEffect(() => {
    if (!resetControls) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const read = async () => {
      try { const next = await api<ResetAutomation>(token, `/accounts/${id}/quota/reset/automation`, undefined, controller.signal); if (!controller.signal.aborted) { setAutomation(next); setAutomationError(false); } }
      catch { if (!controller.signal.aborted) setAutomationError(true); }
      finally { if (!controller.signal.aborted) timer = setTimeout(() => void read(), 15_000); }
    };
    void read();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [id, token, resetControls]);
  useEffect(() => { onBusyChange?.(resetPending || automationBusy || pending); }, [resetPending, automationBusy, pending, onBusyChange]);
  const notify = useToast();
  const [selectedCard, setSelectedCard] = useState('');
  const cards = value?.resetCredits?.credits ?? [];
  const availableCards = cards.filter(card => card.status === 'available' && card.resetType === 'codexRateLimits' && (card.expiresAt == null || card.expiresAt > Date.now())).sort((a, b) => (a.expiresAt ?? Infinity) - (b.expiresAt ?? Infinity));
  const cardId = availableCards.find(card => card.id === selectedCard)?.id ?? availableCards[0]?.id;
  const storedAttempt = automation?.pendingRequest ?? attempt;
  const retry = storedAttempt?.context === value?.resetContext ? storedAttempt : null;
  const canReset = configured && !!automation && !!value?.resetContext && (retry || value.status === 'supported' && !value.stale && !benefitStale(value.resetCredits?.checkedAt) && (value.resetCredits?.availableCount ?? 0) > 0);
  const refresh = async () => {
    setPending(true);
    setRefreshError(null);
    try {
      const next = await api<Quota>(token, `/accounts/${id}/quota/refresh`, {});
      setLatest(next);
      const failure = quotaFailure(next);
      if (failure) notify(failure, 'error');
      else if (next.status !== 'supported' || next.stale) notify('尚未取得新的额度数据，请稍后再试。', 'info');
      else notify('上游额度已刷新。');
    } catch (cause) {
      const message = cause instanceof ApiError ? `${cause.message}（${cause.code}）` : '额度刷新请求未完成，请检查网络和中转站连接。';
      setRefreshError(message);
      notify(message, 'error');
    } finally {
      setPending(false);
    }
  };
  const consume = async () => {
    if (resetPending || !canReset || !value?.resetContext) return;
    const next = retry ?? { context: value.resetContext, idempotencyKey: crypto.randomUUID(), ...(cardId ? { creditId: cardId } : {}) };
    setAttempt(next);
    try { sessionStorage.setItem(`quota-reset:${id}`, JSON.stringify(next)); } catch { /* Keep the same key in memory when storage is unavailable. */ }
    setResetPending(true);
    try {
      const response = await api<{ outcome: string; refreshed: boolean; quota: Quota }>(token, `/accounts/${id}/quota/reset`, next);
      const labels: Record<string, string> = { reset: '已使用一张重置卡。', alreadyRedeemed: '这次重置已完成，没有重复消耗。', nothingToReset: '当前没有可重置的额度窗口。', noCredit: '当前没有可用重置卡。' };
      if (!labels[response.outcome]) throw new Error('无法确认重置结果，请重试核验。');
      setLatest(response.quota); notify(labels[response.outcome] + (response.refreshed ? '额度已重新读取。' : '额度读取失败，请手动刷新。'), response.refreshed ? 'success' : 'error');
      setAttempt(null); try { sessionStorage.removeItem(`quota-reset:${id}`); } catch { /* Optional storage. */ }
      setConfirmReset(false);
    } catch (cause) {
      notify(cause instanceof ApiError && cause.code === 'RESET_UNSUPPORTED' ? '当前原生版本或提供方不支持使用重置卡。' : cause instanceof Error ? cause.message : '无法确认重置结果，请使用原请求重试。', 'error');
    } finally {
      try { setAutomation(await api<ResetAutomation>(token, `/accounts/${id}/quota/reset/automation`)); } catch { setAutomationError(true); }
      setResetPending(false);
    }
  };
  return <section className="account-quota-panel"><div className="section-heading"><h4>上游额度</h4><button className="text-button" disabled={pending || resetPending || !configured} onClick={() => void refresh()}>{pending ? '正在读取…' : '刷新额度'}</button></div>
    <p className={`quota-rate ${value?.status === 'supported' ? quotaTone(remainingWindow, now) : 'neutral'}`}>{quotaLabel(value)}</p><QuotaBenefits quota={value} reset={resetControls ? undefined : { id, token, quota: value, configured }} />
    {refreshError || quotaFailure(value) ? <p className="quota-stale" role="status">{refreshError || quotaFailure(value)}</p> : null}
    {(refreshError || value?.status === 'error') && value?.checkedAt ? <p className="helper">当前显示上次成功读取的数据（{timestamp(value.checkedAt)}）。刷新成功后更新。</p> : null}
    {value?.windows.map(w => {
      const tone = quotaTone(w, now);
      return <div className={`quota-window ${tone}`} key={`${w.bucket}:${w.window}`}>
        <strong>{w.bucket} · {quotaDuration(w.durationMins) ?? '未知'}窗口</strong>
        <span className={`quota-rate ${tone}`}>{w.usedPercent == null ? '未知' : `已用 ${w.usedPercent.toFixed(1)}%`}</span>
        {w.usedPercent != null ? <progress max={100} value={Math.max(0, Math.min(100, w.usedPercent))} aria-label={`${w.bucket} ${w.window} 已用比例`} /> : null}
        <small>重置：{timestamp(w.resetsAt)}</small>
      </div>;
    })}
    {resetControls ? <>
    {cards.length ? <fieldset className="quota-reset-card-list" disabled={resetPending || !!retry}><legend>选择重置卡（每次使用一张）</legend>{cards.map((card, index) => <label className="quota-reset-card" key={card.id}>
      <input type="radio" name={`reset-card-${id}`} aria-label={`选择重置卡 ${card.id}`} value={card.id} checked={card.id === (retry?.creditId ?? cardId)} disabled={!availableCards.some(available => available.id === card.id)} onChange={() => setSelectedCard(card.id)} />
      <span><strong>{card.resetType === 'codexRateLimits' ? 'Codex 重置卡' : '其他类型重置卡'} {index + 1}</strong><small>{card.status === 'available' ? card.expiresAt != null && card.expiresAt <= Date.now() ? '已过期' : '可用' : '不可用'} · {card.expiresAt == null ? '未提供到期时间' : `到期：${timestamp(card.expiresAt)}`}</small><small className="quota-reset-card-id">{card.id}</small></span>
    </label>)}</fieldset> : <p className="helper">上游未提供卡片明细，手动使用时由上游选择一张可用卡片。</p>}
    <div className="quota-reset-mode">
      <label className="field"><span>重置卡使用方式</span><select aria-label="重置卡使用方式" value={automation?.enabled ? 'auto' : 'manual'} disabled={!automation || automationBusy || resetPending} onChange={async event => {
        if (!automation) return;
        const enabled = event.target.value === 'auto';
        setAutomationBusy(true);
        try { setAutomation(await api<ResetAutomation>(token, `/accounts/${id}/quota/reset/automation`, { enabled, revision: automation.revision })); notify(enabled ? '已开启自动使用重置卡。' : '已切换为手动使用重置卡。'); }
        catch (cause) { notify(cause instanceof Error ? cause.message : '保存失败，请稍后重试。', 'error'); }
        finally { setAutomationBusy(false); }
      }}><option value="manual">手动使用（默认）</option><option value="auto">自动使用</option></select></label>
      <p className="helper">自动模式会在重置卡到期前 10 分钟尝试使用，不中断正在执行的请求；关闭页面后仍生效。账户停用时暂停，未提供卡片编号或到期时间的卡片需手动使用。</p>
      {automation?.pendingRequest ? <p className="helper">上次重置结果待确认，将使用原请求核验。切换为手动后仍可手动核验。</p> : null}
      {automationError ? <p className="helper" role="status">重置卡设置读取失败，正在等待重试。</p> : null}
    </div>
    {value?.resetCredits || retry ? <button className="button secondary" disabled={pending || resetPending || !canReset} onClick={() => { setConfirmReset(true); }}>{retry ? '核验上次重置' : '使用重置卡'}</button> : null}
    {confirmReset ? <div className="quota-reset-confirm"><p>{retry ? '继续核验同一次兑换，使用原幂等键，不会另建兑换请求。' : `确认使用${cardId ? `选中的重置卡（${cardId}）` : '一张可用重置卡'}？仅重置上游允许的额度窗口，使用后会重新读取额度。`}</p><button className="button primary" disabled={resetPending || !canReset} onClick={() => void consume()}>{resetPending ? '正在处理…' : retry ? '重试同一次重置' : '确认使用一张'}</button> <button className="button secondary" disabled={resetPending} onClick={() => setConfirmReset(false)}>取消</button></div> : null}
    </> : null}
    <p className="helper">采集：{timestamp(value?.checkedAt)}{value?.stale ? ' · 数据已过期' : ''}</p>
    {value?.lastAttemptAt ? <p className="helper">最近尝试：{timestamp(value.lastAttemptAt)}；额度刷新间隔至少 30 秒。</p> : null}
    {value?.blocked ? <p>已暂停新请求，等待额度恢复确认；管理员启用状态保持不变。</p> : null}
    <p className="helper">这是上游额度；中转站 Token 统计不用于推算此余量。</p>
  </section>;
}
