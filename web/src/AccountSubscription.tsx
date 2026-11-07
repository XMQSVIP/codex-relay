import type { Account, AccountPolicy } from './api';
import { api } from './api';
import { useState } from 'react';
import { Modal } from './ui';

const planNames: Record<string, string> = {
  unknown: '未知',
  free: 'Free', go: 'Go', plus: 'Plus', pro: 'Pro', prolite: 'ProLite', promax: 'ProMax',
  team: 'Team', business: 'Business', enterprise: 'Enterprise', ent26: 'Enterprise', hc: 'Enterprise',
  edu: 'Edu', education: 'Edu', eduplus: 'Edu Plus', edupro: 'Edu Pro',
  selfservebusinessprolite: 'Business ProLite', selfservebusinessusagebased: 'Business Usage Based',
  enterprisecbpautomation: 'Enterprise (Automation)', enterprisecbpusagebased: 'Enterprise CBP Usage Based',
};

export function isApiAccount(account: Account, policy?: AccountPolicy) {
  const auth = policy?.route?.auth;
  return account.source === 'api-key' || account.account?.type === 'apiKey' || auth === 'native-api-key' || auth === 'native-bearer';
}

export function AccountSubscription({ account, policy, token, reload }: { account: Account; policy?: AccountPolicy; token: string; reload: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const apiKey = isApiAccount(account, policy);
  const subscription = account.subscription;
  const auth = policy?.route?.auth;
  const unsupported = apiKey || subscription?.status === 'unsupported' || (auth != null && auth !== 'chatgpt' && auth !== 'native-chatgpt' && !auth.startsWith('native-chatgpt:'));
  const raw = subscription?.planType?.trim() || account.account?.planType?.trim() || account.planType?.trim();
  const key = raw?.toLowerCase().replace(/[\s_-]/g, '') ?? '';
  const label = apiKey ? 'API' : raw ? Object.hasOwn(planNames, key) ? planNames[key] : raw : '未知';
  const tone = apiKey ? 'neutral' : ['pro', 'prolite', 'promax'].includes(key) ? 'subscription-pro' : key === 'plus' ? 'blue' : key === 'free' ? 'success' : 'neutral';
  const badge = <span className={`badge subscription-badge ${tone}`}>{label}</span>;
  const expires = subscription?.expiresAt;
  const stale = subscription?.status === 'stale' || subscription?.status === 'error' || (expires != null && expires <= Date.now());
  const date = (time?: number | null, full = false) => time ? full ? new Date(time).toLocaleString() : `${new Date(time).getFullYear()}-${String(new Date(time).getMonth() + 1).padStart(2, '0')}-${String(new Date(time).getDate()).padStart(2, '0')}` : '未知';
  const awaitingInitial = !expires && !!policy?.enabled && !!policy.route && !subscription?.errorCode
    && (!subscription || subscription.status === 'unknown' || subscription.status === 'error');
  const unknownDate = busy || awaitingInitial ? '正在查询…' : stale && policy?.enabled ? '稍后重试' : '时间未知';
  const retryHint = policy?.enabled
    ? subscription?.nextAttemptAt ? `预计在 ${date(subscription.nextAttemptAt, true)} 后自动重试，也可手动刷新。` : '后台会自动重试，也可手动刷新。'
    : '账户停用期间暂停自动查询，可手动刷新。';
  const refresh = async () => {
    setBusy(true); setError('');
    try { await api(token, `/accounts/${account.id}/subscription/refresh`, {}); await reload(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : '订阅刷新失败'); }
    finally { setBusy(false); }
  };
  if (unsupported) return <span title="此接入不适用 ChatGPT 订阅">{badge}</span>;
  return <>
    <button type="button" className="subscription-trigger" aria-label={`${account.name}的订阅详情`} aria-haspopup="dialog" onClick={() => setOpen(true)}>
      {badge}<span className="subscription-date"><span>到期</span><span>{expires ? date(expires) : unknownDate}</span>{expires && stale ? <span>待确认</span> : null}</span>
    </button>
    {open ? <Modal title="订阅详情" trapFocus busy={busy} close={() => setOpen(false)}>
      <dl className="subscription-details">
        <div><dt>订阅类型</dt><dd>{label}</dd></div>
        <div><dt>开始时间</dt><dd>{date(subscription?.startsAt, true)}</dd></div>
        <div><dt>到期时间</dt><dd>{date(expires, true)}{stale ? '（待确认）' : ''}</dd></div>
        <div><dt>自动续费</dt><dd>{subscription?.willRenew === true ? '开启' : subscription?.willRenew === false ? '关闭' : '未知'}</dd></div>
        <div><dt>最近查询</dt><dd>{date(subscription?.lastAttemptAt, true)}</dd></div>
      </dl>
      <p className="helper">日期表示当前订阅周期结束时间，开启自动续费的账户可能继续续订。时间按本地时区显示。</p>
      {awaitingInitial ? <p role="status" className="helper">正在后台查询订阅到期时间，完成后会自动更新，无需手动刷新。</p> : null}
      {stale && !awaitingInitial ? <p role="status" className="helper">暂时无法确认最新订阅信息，已有结果仍保留；{retryHint}</p> : null}
      {error ? <p role="alert" className="field-error">{error}</p> : null}
      <button type="button" className="button primary" disabled={busy || !policy?.route} onClick={() => void refresh()}>{busy ? '正在刷新…' : '刷新订阅'}</button>
      <p className="helper">刷新请求最短间隔 60 秒。</p>
    </Modal> : null}
  </>;
}
