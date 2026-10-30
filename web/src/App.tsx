import { useState } from 'react';
import { X } from 'lucide-react';
import { api } from './api';
import { ThemePicker, useTheme } from './theme';
import { useConsoleSession } from './useConsoleSession';
import { useConsoleNavigation } from './navigation';
import { ConsoleShell } from './ConsoleShell';
import { StopService } from './StopService';
import { Connect } from './Connect';
import { PasswordLogin } from './AccessSettings';
import { Accounts } from './Accounts';
import { AccountDetails } from './AccountDetails';
import { Groups } from './Operations';
import { ApiKeys } from './ApiKeys';
import { StatisticsPage } from './Statistics';
import { QueuePanel } from './QueuePanel';
import { SystemSettings } from './SystemSettings';
import { Tabs } from './ui';
import { useToast } from './Toast';

export function App() {
  const notify = useToast();
  const theme = useTheme();
  const { search, navigate } = useConsoleNavigation();
  const [stopAccepted, setStopAccepted] = useState(false);
  const state = useConsoleSession(stopAccepted);
  const { token, session, requiresToken, data, connectionError, reload, refresh } = state;
  const params = new URLSearchParams(search);
  const requested = params.get('view');
  const view = requested && ['groups', 'keys', 'requests', 'statistics', 'settings'].includes(requested) ? requested : 'accounts';
  const tab = params.get('tab') ?? '';
  const selected = params.get('account');
  const account = data?.accounts.find(item => item.id === selected);
  const logout = async () => {
    try {
      if (session?.passwordEnabled) await api(token, '/session/logout', {});
      try { sessionStorage.removeItem('bridge-admin'); } catch { /* Memory-only session. */ }
      state.setToken(''); state.recheckSession();
    } catch (cause) { notify(cause instanceof Error ? cause.message : '退出失败', 'error'); }
  };
  const startJob = async (kind: string) => {
    if (state.pending) return;
    state.setPending(kind);
    try { await api(token, '/jobs', { kind }); notify('检查已开始，可在环境与诊断中查看结果。', 'info'); reload(); }
    catch (cause) { notify(cause instanceof Error ? cause.message : '检查未能开始', 'error'); }
    finally { state.setPending(''); }
  };
  if (stopAccepted) return <main className="connect-screen"><section className="connect-panel" role="status"><h1>中转站正在停止</h1><p>服务正在清理任务并退出。再次使用时，请双击“启动中转站”。</p></section></main>;
  if (requiresToken === null) return <main className="connect-screen"><section className="connect-panel"><div className="connect-heading"><img className="brand-symbol" src="/relay-icon-128.png" width={34} height={34} alt="" aria-hidden="true" /><ThemePicker {...theme} /></div><h1>连接管理台</h1>{state.sessionError ? <><p role="alert" className="field-error">{state.sessionError}</p><button className="button primary" onClick={() => state.setSessionAttempt(value => value + 1)}>重试连接</button></> : <p role="status">正在连接本机服务…</p>}</section></main>;
  if (requiresToken && !token) return <Connect theme={theme} serverError={connectionError} onConnect={value => { try { sessionStorage.setItem('bridge-admin', value); } catch { /* Memory-only session. */ } state.setConnectionError(''); state.setToken(value); }} />;
  if (session?.passwordEnabled && !session.authenticated) return <PasswordLogin token={token} theme={theme} onLogin={state.recheckSession} />;
  return <div onClick={navigate}><ConsoleShell view={view} theme={theme} connected={!!data} failed={!!connectionError} refreshing={state.refreshing} reload={reload} logout={requiresToken || session?.passwordEnabled ? () => void logout() : undefined} accountCount={data?.accounts.length ?? 0} version={data?.version} serviceControl={<StopService token={token} disabled={!data || data.operations.draining || !!state.pending} accepted={() => setStopAccepted(true)} />}>
    {connectionError ? <div className="message error" role="alert"><span>{connectionError}</span><button className="text-button" onClick={reload}>重试连接</button><button className="toolbar-icon" aria-label="关闭错误提示" onClick={() => state.setConnectionError('')}><X size={17} /></button></div> : null}
    {data && !data.operations.storageHealthy ? <div className="message error" role="alert">统计存储异常，新的受限请求已暂停。请检查磁盘并重启服务。</div> : null}
    {data?.operations.draining ? <div className="message info" role="status">服务正在退出，已停止接收新请求。</div> : null}
    {!data ? <div className="loading" role="status">{connectionError ? '等待重新连接服务' : '正在读取账户与运行状态…'}</div> : <>
      {view === 'accounts' ? selected ? account ? <AccountDetails key={account.id} account={account} data={data} token={token} section={params.get('section') ?? 'login'} reload={reload} /> : <section className="panel compact-empty"><h2>账户不存在</h2><p>此账户可能已被删除。</p><a className="button secondary" href="?view=accounts">返回账户列表</a></section> : <Accounts data={data} token={token} disabled={!!state.pending} reload={state.refreshAccounts} /> : null}
      {view === 'groups' ? <Groups token={token} operations={data.operations} reload={reload} /> : null}
      {view === 'keys' ? <ApiKeys token={token} operations={data.operations} refreshVersion={refresh} onChange={reload} allowPublicAccess={session?.allowPublicAccess ?? false} /> : null}
      {view === 'requests' ? <><Tabs label="请求监控分类" items={[{ label: '请求记录', href: '?view=requests', active: tab !== 'queue' }, { label: `等待队列 (${data.operations.queued})`, href: '?view=requests&tab=queue', active: tab === 'queue' }]} />{tab === 'queue' ? <QueuePanel token={token} operations={data.operations} refreshVersion={refresh} onChange={reload} /> : <StatisticsPage mode="records" token={token} operations={data.operations} refreshVersion={refresh} />}</> : null}
      {view === 'statistics' ? <StatisticsPage mode="summary" token={token} operations={data.operations} refreshVersion={refresh} /> : null}
      {view === 'settings' ? <SystemSettings token={token} data={data} tab={tab} reload={reload} startJob={kind => void startJob(kind)} onAccessSaved={config => { state.setSession(value => value ? { ...value, ...config, authenticated: true } : value); reload(); }} /> : null}
    </>}
  </ConsoleShell></div>;
}
