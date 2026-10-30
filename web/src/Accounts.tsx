import { accountGroupIds, availableAccountGroup, sameGroups } from './accountMembership';
import { AccountGroupControl } from './AccountGroups';
import { useToast } from './Toast';
import { useEffect, useId, useRef, useState } from 'react';
import { ArrowRight, Layers3, LoaderCircle, Search, Settings2, ShieldCheck, Users } from 'lucide-react';
import { api } from './api';
import type { Account, AccountPolicy, Group, Overview } from './api';
import { nativeAuthLabel } from './nativeAuth';
import { CreateAccount } from './CreateAccount';
import { DeleteAccount } from './DeleteAccount';
import { AccountActivationHelp, AccountActivationSwitch } from './AccountActivationSwitch';
import { useQueryState, useQueryNumber } from './navigation';
import { accountStatus } from './accountStatus';
import { AccountInsights, useAccountMetrics, timestamp } from './AccountInsights';
import { TokenCount } from './TokenCount';
import './account-management.css';
import { AccountQuotaCell } from './AccountQuota';
import { AccountSubscription, isApiAccount } from './AccountSubscription';
import { AccountBatch } from './AccountBatch';
import { AccountImport } from './AccountImport';
import { proxyModeLabel, proxyStatusLabel } from './ProxySettings';
import './account-proxy-summary.css';
import { AccountTestChatButton } from './AccountTestChat';

type Tone = 'neutral' | 'success' | 'warning' | 'blue';
type Status = { label: string; detail: string; tone: Tone };
type AccountAction = 'enable' | 'disable' | 'group';

export function accountProxyLabel(policy: AccountPolicy, automatic: Overview['automaticProxy']) {
  const mode = policy.proxy?.mode ?? 'auto';
  if (mode === 'direct') return proxyModeLabel.direct;
  if (mode === 'custom') return `${proxyModeLabel.custom} · ${policy.proxy?.url ?? ''}`;
  if (automatic.state === 'error') return '自动 · 代理解析失败';
  if (automatic.state === 'direct') return '自动 · 直连';
  if (automatic.http && automatic.http === automatic.https) return `自动 · ${automatic.http}`;
  return `自动 · ${[automatic.http ? `HTTP ${automatic.http}` : '', automatic.https ? `HTTPS ${automatic.https}` : ''].filter(Boolean).join(' · ')}`;
}

const credentialNames = { 'api-key': 'API Key · 直接连接上游', managed: '原生凭据 · 自动续期', external: 'Access Token · 手动续期', desktop: '已绑定本机 Codex 登录', 'native-login': '独立原生登录' };
export const accountSettingsUrl = (account: Account, section?: string) => {
  const params = new URLSearchParams((new URLSearchParams(location.search).get('view') ?? 'accounts') === 'accounts' ? location.search : 'view=accounts');
  params.set('view', 'accounts'); params.set('account', account.id);
  if (section) params.set('section', section); else params.delete('section');
  return `?${params}`;
};

function hasCredentials(account: Account) {
  if (account.source === 'api-key') return !!account.upstream?.hasApiKey;
  return account.source === 'native-config' || !!account.credentialSource || !!account.account;
}

function hasProcess(account: Account) {
  return !!account.pid || account.state === 'generating';
}

function loginStatus(account: Account, policy?: AccountPolicy): Status {
  if (account.source === 'api-key') return { label: 'API Key', detail: account.upstream?.baseUrl ?? '直接连接上游', tone: 'success' };
  if (account.source === 'native-config') return { label: '本机 Codex 配置', detail: policy?.route ? nativeAuthLabel(policy.route.auth) : '自动识别本机提供方与凭证', tone: 'neutral' };
  if (account.source === 'shared-desktop') return { label: '桌面登录', detail: '仅用于本机调试', tone: 'neutral' };
  if (account.account) return { label: '已登录', detail: account.account.email ?? '独立账户已登录', tone: 'success' };
  if (account.credentialSource) return { label: '已配置凭据', detail: credentialNames[account.credentialSource], tone: 'success' };
  return { label: '未登录', detail: '登录或导入凭据后，可启用接入', tone: 'neutral' };
}

function runtimeStatus(account: Account, policy?: AccountPolicy): Status {
  if (account.source === 'api-key') return { label: policy?.active ? '请求执行中' : '直连就绪', detail: account.upstream?.transport === 'ws-first' ? 'WS 优先' : 'HTTP / SSE', tone: 'neutral' };
  if (account.state === 'generating') return { label: '请求执行中', detail: policy?.enabled === false ? '当前任务完成后停止实例' : `${policy?.active || 1} 个任务正在运行`, tone: 'blue' };
  if (account.state === 'starting') return { label: '操作中', detail: '等待实例完成操作', tone: 'blue' };
  if (account.state === 'unavailable' || account.state === 'disconnected') return { label: '状态异常', detail: account.state === 'unavailable' ? '无法读取实例状态，请刷新重试' : '原生进程已断开', tone: 'warning' };
  if (account.pid) return { label: '运行中', detail: `PID ${account.pid}`, tone: 'success' };
  return policy?.route && policy.enabled ? { label: '待命', detail: '收到请求时自动启动', tone: 'neutral' } : { label: '未启动', detail: '启用账户后自动启动一个实例', tone: 'neutral' };
}

function accessStatus(account: Account, policy?: AccountPolicy, group?: Group): Status {
  return accountStatus(account, policy, group);
}

function StatusBadge({ status }: { status: Status }) {
  return <span className={`badge ${status.tone}`}><span className="status-dot" aria-hidden="true" />{status.label}</span>;
}

export function AccountSummary({ account, policy, group }: { account: Account; policy?: AccountPolicy; group?: Group }) {
  const login = loginStatus(account, policy);
  const runtime = runtimeStatus(account, policy);
  const access = accessStatus(account, policy, group);
  if (account.source === 'native-config') return <div className="native-status-strip" aria-label="账户状态">
    {([['账户状态', access], ['实例运行', runtime]] as const).map(([label, status]) => <div key={label} className="native-status-item"><div><span>{label}</span><StatusBadge status={status} /></div><small>{status.detail}</small></div>)}
  </div>;
  const statuses = [['登录与凭据', login], [account.source === 'api-key' ? '上游连接' : '实例运行', runtime], ['账户状态', access]] as const;
  const nextStep = login.label === '未登录'
    ? { message: '先完成登录或导入凭据，再启用账户。', label: '去登录', section: 'login' }
    : ['未启用', '已停用'].includes(access.label)
      ? { message: '凭据已就绪，下一步启用账户。', label: '去启用账户', section: 'access' }
      : null;
  return <div className="account-detail-summary">
    <div className="account-summary-grid">
      {statuses.map(([label, status]) => {
        const action = label === '登录与凭据' && status.label === '未登录'
          ? { label: '去登录', section: 'login' }
          : label === '账户状态' && ['未启用', '已停用'].includes(status.label)
            ? { label: '去启用', section: 'access' }
            : null;
        return <div key={label} className="account-summary-item"><span>{label}</span><StatusBadge status={status} /><small>{status.detail}</small>{action ? <a className="account-summary-action" href={accountSettingsUrl(account, action.section)}>{action.label}<ArrowRight size={13} aria-hidden="true" /></a> : null}</div>;
      })}
    </div>
    {nextStep ? <div className="account-next-step"><span className="account-next-step-label">下一步</span><span>{nextStep.message}</span><a className="account-summary-action" href={accountSettingsUrl(account, nextStep.section)}>{nextStep.label}<ArrowRight size={13} aria-hidden="true" /></a></div> : null}
  </div>;
}

export function Accounts({ data, token, disabled, reload }: { data: Overview; token: string; disabled: boolean; reload: () => Promise<void> }) {
  const prefix = useId();
  const [query, setQuery] = useQueryState('q', '');
  const [filter, setFilter] = useQueryState('state', 'all');
  const [page, setPage] = useQueryNumber('page', 1);
  const [groupFilter, setGroupFilter] = useQueryState('groupId', '');
  const [expanded, setExpanded] = useQueryState('expanded', '');
  const [metricsVersion, setMetricsVersion] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const refreshInFlight = useRef(false);
  const [selected, setSelected] = useState<string[]>([]);
  useEffect(() => setSelected([]), [query, filter, groupFilter]);
  const metrics = useAccountMetrics(token, '', 0);
  const [pending, setPending] = useState<{ id: string; action: AccountAction; groupIds?: string[] } | null>(null);
  const notify = useToast();
  const inFlight = useRef(false);
  const policies = new Map(data.operations.accounts.map(policy => [policy.id, policy]));
  const groups = new Map(data.operations.groups.map(group => [group.id, group]));
  const pool = data.accounts.filter(account => account.source !== 'shared-desktop');
  const term = query.trim().toLocaleLowerCase();
  const matchesFilter = (account: Account, value: string) => {
    const policy = policies.get(account.id);
    const category = accountStatus(account, policy, availableAccountGroup(policy, data.operations.groups)).category;
    return value === 'all' || category === value || value === 'enabled' && category === 'available' || value === 'running' && hasProcess(account) || value === 'unconfigured' && !policy?.route;
  };
  const visible = data.accounts.filter(account => {
    const policy = policies.get(account.id);
    const text = [account.name, account.id, account.account?.email, ...accountGroupIds(policy).map(id => groups.get(id)?.name)].filter(Boolean).join(' ').toLocaleLowerCase();
    return account.source !== 'shared-desktop' && (!term || text.includes(term)) && (!groupFilter || accountGroupIds(policy).includes(groupFilter)) && matchesFilter(account, filter);
  });
  const pages = Math.max(1, Math.ceil(visible.length / 20));
  const currentPage = Math.min(page, pages);
  const busy = disabled || !!pending;
  useEffect(() => {
    if (location.hash.startsWith('#account-')) document.getElementById(location.hash.slice(1))?.focus();
  }, []);

  const refreshAccounts = async () => {
    if (refreshInFlight.current || busy) return;
    refreshInFlight.current = true;
    setRefreshing(true);
    setMetricsVersion(value => value + 1);
    try {
      const [overview, usage] = await Promise.allSettled([reload(), metrics.reload()]);
      const failures = [
        overview.status === 'rejected' ? `账号状态读取失败${overview.reason instanceof Error ? `（${overview.reason.message}）` : ''}` : '',
        usage.status === 'rejected' || !usage.value ? '今日用量读取失败' : '',
      ].filter(Boolean);
      if (failures.length) notify(`账号数据刷新未完成：${failures.join('；')}。请重试。`, 'error');
      else notify('账号数据已刷新。', 'success');
    } finally {
      refreshInFlight.current = false;
      setRefreshing(false);
    }
  };

  const run = async (account: Account, kind: AccountAction, values?: { groupIds: string[]; revision: number; enabled: boolean; maxConcurrency: number }) => {
    const groupIds = values?.groupIds;
    if (inFlight.current || disabled) return false;
    const policy = policies.get(account.id);
    const nextGroups = data.operations.groups.filter(group => groupIds?.includes(group.id));
    if (kind === 'group' && (!policy || !values || !nextGroups.length || policy.deleting || !data.operations.storageHealthy || data.operations.draining || !sameGroups(groupIds ?? [], accountGroupIds(policy)) && (policy.active || policy.queued))) return false;
    inFlight.current = true;
    setPending({ id: account.id, action: kind, groupIds });
    let succeeded = false;
    try {
      const isPolicy = kind === 'group';
      const result = await api<{ warning?: string }>(token, `/accounts/${account.id}/${isPolicy ? 'policy' : 'activation'}`, isPolicy ? { revision: data.operations.revision, ...values } : { revision: data.operations.revision, enabled: kind === 'enable' });
      succeeded = true;
      const messages = { enable: '账户已启用，正在自动启动一个实例。', disable: '账户已停用，不再接收新请求；当前任务完成后自动停止实例。', group: `所属分组已保存为“${nextGroups.map(group => group.name).join("、")}”。${nextGroups.every(group => !group.enabled) ? '所选分组均已暂停，此账户暂不接收新请求。' : ''}` };
      notify(`${account.name}：${result.warning ?? messages[kind]}`, result.warning ? 'error' : 'success');
    } catch (error) {
      notify(error instanceof TypeError ? '无法连接服务，请检查连接后刷新状态。' : error instanceof Error ? error.message : '操作未完成，请重试。', 'error');
    }
    try { await reload(); }
    catch { if (succeeded) notify('操作已完成，但状态刷新失败。请点击“刷新状态”重新读取。', 'error'); }
    finally { inFlight.current = false; setPending(null); }
    return succeeded;
  };

  return <div className="account-list-page">
    <section className="panel account-console" aria-labelledby={`${prefix}-title`}>
      <h2 id={`${prefix}-title`} className="visually-hidden">账户列表</h2>
      <div className="account-console-toolbar">
        <label className="account-search"><Search size={17} aria-hidden="true" /><span className="sr-only">搜索账户</span><input type="search" placeholder="搜索名称、邮箱或分组" value={query} onChange={event => { setQuery(event.target.value); setPage(1); }} disabled={!!pending} /></label>
        <label className="account-filter-select"><span className="visually-hidden">筛选账户状态</span><select aria-label="筛选账户状态" value={filter} onChange={event => { setFilter(event.target.value); setPage(1); }}>{[['all', '全部状态'], ['available', '可调度'], ['limited', '暂时受限'], ['pending', '待处理'], ['disabled', '已停用'], ['paused', '分组已暂停'], ['unconfigured', '待配置'], ['running', '运行中']].map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label className="account-filter-select"><span className="visually-hidden">筛选所属分组</span><select aria-label="筛选所属分组" value={groupFilter} onChange={event => { setGroupFilter(event.target.value); setPage(1); }}><option value="">全部分组</option>{data.operations.groups.map(group => <option key={group.id} value={group.id}>{group.name}</option>)}</select></label>
        <div className="account-list-count" role="status" aria-atomic="true"><span>显示 {visible.length} 个账户</span></div>
        <div className="account-heading-actions"><button type="button" className="text-button account-refresh-button" disabled={busy || refreshing} aria-busy={refreshing} onClick={() => void refreshAccounts()}>{refreshing ? <LoaderCircle size={15} className="spin" aria-hidden="true" /> : null}{refreshing ? '正在刷新账号数据…' : '刷新账号数据'}</button><a href="?view=groups" className="text-button"><Layers3 size={15} aria-hidden="true" />管理分组</a><AccountImport token={token} operations={data.operations} disabled={busy || data.operations.draining || !data.operations.storageHealthy} reload={reload} /><CreateAccount token={token} disabled={busy || data.operations.draining} operations={data.operations} accounts={data.accounts} onChange={reload} /></div>
        {query || filter !== 'all' || groupFilter ? <button className="text-button" onClick={() => { setQuery(''); setFilter('all'); setGroupFilter(''); setPage(1); }}>清除筛选</button> : null}
      </div>
      <AccountBatch token={token} accounts={pool} operations={data.operations} selected={selected} clear={() => setSelected([])} reload={reload} disabled={busy || data.operations.draining} />
      {visible.length ? <label className="batch-page-select"><input type="checkbox" aria-label="选择当前页全部账号" checked={visible.slice((currentPage - 1) * 20, currentPage * 20).every(a => selected.includes(a.id))} onChange={event => { const ids = visible.slice((currentPage - 1) * 20, currentPage * 20).map(a => a.id); setSelected(old => event.target.checked ? [...new Set([...old, ...ids])] : old.filter(id => !ids.includes(id))); }} />选择当前页</label> : null}
      {data.accounts.length ? <>
        {metrics.error ? <p className="message error" role="alert">{metrics.error}</p> : null}
        <div className="account-column-headings" aria-hidden="true"><span>账户</span><span>周期用量</span><span>订阅</span><span>所属分组</span><span>接入状态</span><span>账户并发</span><span>今日 Token</span><span>操作</span></div>
        <ul className="account-console-list">{visible.slice((currentPage - 1) * 20, currentPage * 20).map(account => {
          const policy = policies.get(account.id);
          const group = availableAccountGroup(policy, data.operations.groups);
          const login = loginStatus(account, policy);
          const metric = metrics.data?.accounts.find(row => row.id === account.id);
          const desktop = account.source === 'shared-desktop';
          const live = hasProcess(account);
          const working = !!policy?.active || !!policy?.queued || account.state === 'starting';
          const ownPending = pending?.id === account.id ? pending.action : null;
          const canStart = !working && data.operations.storageHealthy;
          const accountEnabled = !!policy?.route && policy.enabled;
          const canEnable = account.source === 'api-key' ? !!account.upstream?.hasApiKey : !!policy?.route || canStart && hasCredentials(account);
          const unavailable = busy || data.operations.draining || !!policy?.deleting;
          const accessPending = ownPending === 'enable' || ownPending === 'disable' ? ownPending : null;
          const access: Status = accessPending === 'enable'
            ? { label: '启用中', detail: '正在启用账户，请稍候', tone: 'blue' }
            : accessPending === 'disable'
              ? { label: '停用中', detail: '正在停用账户，请稍候', tone: 'blue' }
              : accessStatus(account, policy, group);
          return <li key={account.id}><article className="account-console-row" id={`account-${account.id}`} tabIndex={-1} aria-labelledby={`${prefix}-${account.id}`} aria-busy={!!ownPending}>
            <div className="account-identity">
              <input className="account-selection" type="checkbox" aria-label={`选择账号：${account.name}`} checked={selected.includes(account.id)} onChange={event => setSelected(old => event.target.checked ? [...old, account.id] : old.filter(id => id !== account.id))} />
              <div className="account-identity-body">
                <div className="account-name-line"><h3 id={`${prefix}-${account.id}`}><a href={accountSettingsUrl(account)} title={account.name}>{account.name}</a></h3><StatusBadge status={login} /></div>
                <div className="account-identity-meta">{hasCredentials(account) ? <span className="account-email" title={login.detail}>{login.detail}</span> : <code title={account.id}>{account.id.slice(0, 8)}</code>}</div>
                {account.clients?.length ? <div className="account-identity-meta"><span>{[...new Set(account.clients.map(client => `${client.clientInfo.name} ${client.clientInfo.version}`))].join(' / ')}</span></div> : null}

              </div>
            </div>
            <div className="account-control-cell account-quota-cell" aria-label={`${account.name}的周期用量`}><span className="account-column-label">周期用量</span><AccountQuotaCell id={account.id} token={token} configured={!!policy?.route} quota={policy?.quota} apiAccount={isApiAccount(account, policy)} /></div>
            <div className="account-control-cell account-subscription-cell" aria-label={`${account.name}的订阅类型`}><span className="account-column-label">订阅</span><AccountSubscription key={`${account.id}:${policy?.route?.auth}:${policy?.route?.baseUrl}`} account={account} policy={policy} token={token} reload={reload} /></div>
            <div className="account-group-cell"><span className="account-column-label">所属分组</span>
                {!desktop && policy ? <div className="account-group-control">
                  <AccountGroupControl name={account.name} groups={data.operations.groups} value={accountGroupIds(policy)} revision={data.operations.revision} disabled={unavailable || working || !data.operations.storageHealthy} busy={ownPending === 'group'} save={(groupIds, revision) => run(account, 'group', { groupIds, revision, enabled: policy.enabled, maxConcurrency: policy.maxConcurrency })} />
                  {ownPending === 'group' ? <LoaderCircle size={14} className="spin" aria-label="正在保存分组" /> : null}
                  {working ? <span className="account-group-hint" id={`${prefix}-${account.id}-group-hint`}>请求结束后可更改分组</span> : null}
                </div> : null}
              {desktop || !policy ? <span className="helper">—</span> : null}
            </div>
            <div className="account-control-cell account-runtime-cell">
              <span className="account-column-label">接入状态</span>
              <StatusBadge status={access} />
              {!desktop ? <AccountActivationSwitch name={account.name} enabled={accountEnabled} pending={accessPending} disabled={unavailable || !policy || !data.operations.storageHealthy || !accountEnabled && !canEnable} describedBy={access.label !== '可调度' ? `${prefix}-${account.id}-activation-hint` : undefined} onChange={value => void run(account, value ? 'enable' : 'disable')} /> : null}
              <AccountActivationHelp account={account} configured={!!policy?.route} installationReady={!!data.installation} />
              {access.label !== '可调度' ? <p id={`${prefix}-${account.id}-activation-hint`}>{access.detail}</p> : null}{!desktop && policy ? <a className="account-action-hint account-proxy-summary" href={`${accountSettingsUrl(account, 'access')}#account-proxy`} aria-label={`${account.name}的代理设置`} title={accountProxyLabel(policy, data.automaticProxy)}>代理：{accountProxyLabel(policy, data.automaticProxy)}{policy.proxyStatus && policy.proxyStatus.state !== 'applied' ? ` · ${proxyStatusLabel[policy.proxyStatus.state]}` : ''}</a> : null}
              {!desktop && account.source !== 'api-key' && !!policy?.route && !live && !data.installation ? <a href="?view=settings&tab=diagnostics" className="account-action-hint">先检测原生安装</a> : null}
            </div>
            <div className="account-control-cell account-concurrency-cell">
              <span className="account-column-label">账户并发</span>
              {policy && !desktop ? <><strong>{policy.active}<span> / {policy.maxConcurrency}</span></strong><p>{policy.queued ? `${policy.queued} 个请求排队` : policy.active >= policy.maxConcurrency ? '并发已满' : '可调整上限'}</p></> : <p>仅用于调试</p>}
            </div>
            <div className="account-control-cell account-usage-cell"><span className="account-column-label">今日 Token</span><strong><TokenCount value={metric?.totalTokens} /></strong><p title={`保留 ${data.operations.retentionDays} 天内的最后一次推理`}>{timestamp(metric?.lastUsedAt)}</p></div>
            <div className="account-row-actions" aria-label={`${account.name}的操作`}>
              {!desktop && policy ? <AccountTestChatButton iconOnly account={account} policy={policy} proxy={accountProxyLabel(policy, data.automaticProxy)} token={token} disabled={unavailable || !data.operations.storageHealthy} /> : null}
              <button className="account-icon-button" aria-label={`展开详情：${account.name}`} aria-expanded={expanded === account.id} aria-controls={`insights-${account.id}`} onClick={() => setExpanded(expanded === account.id ? '' : account.id)}>{expanded === account.id ? '−' : '+'}</button>
              <a className="account-icon-button account-settings-link" title="账户设置" href={accountSettingsUrl(account)} aria-label={`账户设置：${account.name}`}><Settings2 size={18} aria-hidden="true" /></a>
              {!desktop && policy ? <DeleteAccount iconOnly token={token} account={policy} revision={data.operations.revision} disabled={busy || data.operations.draining} onChange={() => { void reload().catch(() => notify('账户状态刷新失败，请点击“刷新状态”重试。', 'error')); }} /> : null}
            </div>
          </article>{expanded === account.id ? <div id={`insights-${account.id}`}><AccountInsights account={account} policy={policy} token={token} refresh={metricsVersion} retentionDays={data.operations.retentionDays} /></div> : null}</li>;
        })}</ul>
        {!visible.length ? <div className="account-no-results"><Search size={24} aria-hidden="true" /><h3>没有匹配的账户</h3><p>试试其他名称、邮箱或分组。</p><button className="button secondary" onClick={() => { setQuery(''); setFilter('all'); setGroupFilter(''); setPage(1); }}>清除筛选</button></div> : null}
      </> : <div className="empty-state"><div className="empty-icon"><Users size={31} aria-hidden="true" /></div><h3>添加第一个账户</h3><p>点击“添加账户”，使用本机 Codex 一键连接，或登录其他账户、导入凭据。</p></div>}
      <div className="account-console-foot"><ShieldCheck size={15} aria-hidden="true" /><p>启用账户即可接收请求，后台自动启动一个实例；关闭后不再接收新请求，当前任务完成后自动停止。</p></div>
    </section>
    {visible.length > 20 ? <div className="pagination"><span>第 {currentPage} / {pages} 页</span><button className="button secondary" disabled={currentPage <= 1} onClick={() => setPage(currentPage - 1)}>上一页</button><button className="button secondary" disabled={currentPage >= pages} onClick={() => setPage(currentPage + 1)}>下一页</button></div> : null}
  </div>;
}
