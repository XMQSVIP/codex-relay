import { availableAccountGroup } from './accountMembership';
import { useToast } from './Toast';
import { useEffect, useId, useRef, useState } from 'react';
import { Check, LoaderCircle, Monitor, RefreshCw } from 'lucide-react';
import { api, ApiError } from './api';
import type { Account, Operations } from './api';
import { nativeAuthLabel } from './nativeAuth';

interface Inspection { id: string; provider: string; auth: string; model: string; endpoint: string | null }
interface Connected { status: 'enabled' | 'pending' | 'changed'; account?: Account; inspection?: Inspection; message?: string; error?: { code: string; message: string } }
const defaultGroup = '00000000-0000-0000-0000-000000000000';
function recovery(error: unknown): string {
  if (error instanceof ApiError) {
    if (['LAUNCH_CONFIG', 'DESKTOP_SELECTION', 'DESKTOP_BACKEND', 'WINDOWS_DISCOVERY', 'DESKTOP_INSTALLATION_STALE'].includes(error.code)) return '请启动官方 Codex 桌面并保持运行，然后点击“重新检测”。';
    if (['PROVIDER_AUTH_MISSING', 'OAUTH_NOT_SIGNED_IN'].includes(error.code)) return '请在本机 Codex 完成登录或配置提供方凭据，再重新检测；也可以选择登录其他 ChatGPT 账户。';
    if (error.code === 'CONFIG_CONFLICT') return '账户或分组设置已更新，请检查当前分组后重试。';
  }
  return error instanceof Error ? error.message : '无法连接服务，请检查连接后重试。';
}

export function LocalConnection({ token, operations, accounts, target, disabled, onChange, onBusyChange }: {
  token: string; operations: Operations; accounts: Account[]; target?: Account; disabled: boolean; onChange: () => void | Promise<void>; onBusyChange?: (busy: boolean) => void;
}) {
  const prefix = useId();
  const policy = target ? operations.accounts.find(a => a.id === target.id) : undefined;
  const [name, setName] = useState(target?.name ?? '本机 Codex');
  const [group, setGroup] = useState(policy?.groupIds[0] ?? defaultGroup);
  const [inspection, setInspection] = useState<Inspection | null>(null);
  const [checking, setChecking] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState('');
  const notify = useToast();
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<Connected | null>(null);
  const [another, setAnother] = useState(false);
  const inFlight = useRef(false);
  const storageKey = `local-connection:${target?.id ?? 'new'}`;
  const [requestId] = useState(() => {
    try { const stored = sessionStorage.getItem(storageKey); if (stored && /^[0-9a-f-]{36}$/.test(stored)) return stored; } catch { /* Optional recovery storage. */ }
    const id = crypto.randomUUID();
    try { sessionStorage.setItem(storageKey, id); } catch { /* Retry still works while mounted. */ }
    return id;
  });
  useEffect(() => { onBusyChange?.(connecting); return () => onBusyChange?.(false); }, [connecting, onBusyChange]);
  const existing = accounts.filter(a => a.source === 'native-config' && a.id !== requestId);
  const chooseExisting = !target && existing.length > 0 && !another && !result;
  const selectedGroup = policy ? availableAccountGroup(policy, operations.groups) : operations.groups.find(g => g.id === group);
  const busy = disabled || checking || connecting;
  useEffect(() => {
    const controller = new AbortController();
    setChecking(true); setError(''); setInspection(null);
    void api<Inspection>(token, '/local-connection/inspect', { accountId: target?.id }, controller.signal).then(value => {
      if (!controller.signal.aborted) setInspection(value);
    }).catch(cause => {
      if (!controller.signal.aborted) setError(recovery(cause));
    }).finally(() => { if (!controller.signal.aborted) setChecking(false); });
    return () => controller.abort();
  }, [token, attempt, target?.id]);
  const connect = async () => {
    if (!inspection || inFlight.current || busy || !selectedGroup?.enabled) return;
    inFlight.current = true; setConnecting(true); setError('');
    try {
      const response = await api<Connected>(token, '/local-connection/connect', {
        requestId, inspectionId: inspection.id, ...(target ? { accountId: target.id } : {}), name: name.trim(), groupId: selectedGroup.id, revision: operations.revision,
      });
      if (response.status === 'changed') {
        setInspection(response.inspection ?? null); notify(response.message ?? '本机配置已变化，请确认后再次连接。', 'info');
      } else {
        setResult(response);
        if (response.error) notify(response.error.message, 'error');
        if (response.status === 'enabled') { notify(`“${response.account?.name ?? name}”已连接并启用。`); try { sessionStorage.removeItem(storageKey); } catch { /* Optional storage. */ } }
      }
    } catch (cause) {
      notify(recovery(cause), 'error');
      if (cause instanceof ApiError && cause.code === 'LOCAL_INSPECTION_EXPIRED') setInspection(null);
    } finally {
      try { await onChange(); } catch { notify('请刷新账户状态，已完成的连接不会重复创建。', 'error'); }
      inFlight.current = false; setConnecting(false);
    }
  };
  if (result?.status === 'enabled') return <div className="local-connection-success" role="status">
    <Check size={22} aria-hidden="true" /><h3>{result.account?.name ?? name} · 已连接并启用</h3>
    <p>接入启用后会自动启动一个实例。同分组已有 API Key 可继续使用。</p>
    <div className="key-actions"><a className="button primary" href={`?view=keys&create=1&group=${encodeURIComponent(selectedGroup?.id ?? group)}`}>创建 API Key</a><a className="button secondary" href={`?view=accounts&account=${result.account?.id}`}>查看账户</a></div>
    {error ? <p className="helper">接入已启用，但能力信息更新失败：{error}</p> : null}
  </div>;
  return <form className="local-connection" aria-busy={checking || connecting} onSubmit={event => { event.preventDefault(); void connect(); }}>
    <p className="helper">自动使用本机 Codex 当前的账户或提供方配置。检测和启用不会发送模型测试请求。</p>
    {chooseExisting ? <div className="local-existing"><strong>已有本机账户</strong>{existing.map(account => <a key={account.id} className="button secondary" href={`?view=accounts&account=${account.id}`}>管理 {account.name}</a>)}<button type="button" className="text-button" onClick={() => setAnother(true)}>添加另一个实例</button><p className="helper">多个实例读取同一本机配置，共享上游账户与额度。</p></div> : null}
    <div className="local-detection" role="status">{checking ? <p><LoaderCircle className="spin" size={16} aria-hidden="true" /> 正在检测本机 Codex…</p> : inspection ? <><strong><Monitor size={17} aria-hidden="true" /> 已识别本机配置</strong><dl><dt>提供方</dt><dd>{inspection.provider}</dd><dt>连接方式</dt><dd>{nativeAuthLabel(inspection.auth)}</dd><dt>默认模型</dt><dd>{inspection.model}</dd>{inspection.endpoint ? <><dt>服务地址</dt><dd>{inspection.endpoint}</dd></> : null}</dl></> : <p>尚未取得可用的本机连接配置。</p>}</div>

    {error ? <p className="field-error" role="alert">{error}</p> : null}
    {result?.status === 'pending' ? <p role="status">{result.message ?? `${result.account?.name} 已添加，尚未启用。重试会继续使用这个账户。`}</p> : null}
    {!chooseExisting ? <><div className="account-field"><label htmlFor={`${prefix}-name`}>账户名称</label><input id={`${prefix}-name`} value={name} onChange={event => setName(event.target.value)} maxLength={60} required disabled={busy || !!target || !!result?.account} /></div>
      <div className="account-field"><label htmlFor={`${prefix}-group`}>所属分组</label><select id={`${prefix}-group`} value={selectedGroup?.id ?? group} onChange={event => setGroup(event.target.value)} disabled={busy || !!target}>{operations.groups.map(item => <option key={item.id} value={item.id}>{item.name}{item.enabled ? '' : '（已暂停）'}</option>)}</select></div>
      {!selectedGroup?.enabled ? <p className="field-error" role="alert">所选分组已暂停或不存在。{target ? '请先在分组管理中恢复，或在账户列表调整分组。' : '请选择启用的分组。'}<a href="?view=groups">管理分组</a></p> : null}</> : null}
    <div className="account-dialog-actions"><button type="button" className="button secondary" disabled={busy} onClick={() => {  setAttempt(value => value + 1); }}><RefreshCw size={15} aria-hidden="true" />重新检测</button>{!chooseExisting ? <button type="submit" className="button primary" disabled={busy || !inspection || !name.trim() || !selectedGroup?.enabled}>{connecting ? <LoaderCircle className="spin" size={15} aria-hidden="true" /> : null}{connecting ? '正在连接并启用…' : result?.status === 'pending' ? '重试启用' : target?.source === 'native-config' ? '更新并启用' : '连接并启用'}</button> : null}</div>
  </form>;
}
