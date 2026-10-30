import { useEffect, useRef, useState } from 'react';
import { ExternalLink, KeyRound, LogIn } from 'lucide-react';
import { api, ApiError } from './api';
import type { Login } from './api';
import { copyText } from './clipboard';
import './account-login.css';

const active = (login: Login | null) => login?.status === 'waiting' || login?.status === 'completing';
const titles: Record<Login['status'], string> = { waiting: '等待浏览器授权', completing: '正在确认登录结果', succeeded: '登录成功，凭据已保存', failed: '登录失败', cancelled: '本次登录已取消', expired: '登录会话已失效，请重新发起' };

export function AccountLogin({ id, token, disabled, onChange, onActiveChange }: {
  id: string; token: string; disabled: boolean; onChange: () => void; onActiveChange: (active: boolean) => void;
}) {
  const [login, setLogin] = useState<Login | null>(null);
  const [pending, setPending] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState('');
  const [connection, setConnection] = useState('');
  const [callback, setCallback] = useState('');
  const [copied, setCopied] = useState('');
  const [cycle, setCycle] = useState(0);
  const [now, setNow] = useState(Date.now);
  const generation = useRef(0);
  const expectedId = useRef<string | null>(null);
  const reported = useRef<string | null>(null);
  const busy = active(login);
  const path = `/accounts/${id}/login`;

  useEffect(() => {
    const controller = new AbortController();
    const current = generation.current;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      let again = true;
      try {
        const query = expectedId.current ? `?loginId=${encodeURIComponent(expectedId.current)}` : '';
        const result = await api<Login | null>(token, path + query, undefined, controller.signal);
        if (controller.signal.aborted || current !== generation.current) return;
        setLogin(result); setLoaded(true); setConnection('');
        if (result) expectedId.current = result.loginId;
        again = active(result);
      } catch (cause) {
        if (controller.signal.aborted || current !== generation.current) return;
        if (cause instanceof ApiError && cause.code === 'LOGIN_EXPIRED') {
          setLogin(null); expectedId.current = null; setLoaded(true);
          setError('登录会话已失效，请重新发起。'); setConnection(''); again = false;
        } else { setConnection('状态连接中断，正在重试；这不代表授权失败。'); }
      } finally {
        if (again && !controller.signal.aborted && current === generation.current) timer = setTimeout(() => void poll(), 2000);
      }
    };
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [token, path, cycle]);

  useEffect(() => { onActiveChange(busy); return () => onActiveChange(false); }, [busy, onActiveChange]);
  useEffect(() => {
    if (!busy) { setCallback(''); return; }
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [busy]);
  useEffect(() => {
    if (login && !active(login) && reported.current !== login.loginId) {
      reported.current = login.loginId; setCallback(''); onChange();
    }
  }, [login, onChange]);

  const run = async (action: string, body: unknown) => {
    generation.current++; setPending(action); setError(''); setCopied('');
    try {
      if (action === 'cancel') {
        await api(token, `${path}/cancel`, body);
        const result = await api<Login>(token, `${path}?loginId=${encodeURIComponent(expectedId.current!)}`);
        setLogin(result);
        setCallback('');
      } else {
        const result = await api<Login>(token, path + (action === 'callback' ? '/callback' : ''), body);
        expectedId.current = result.loginId; setLogin(result); setLoaded(true);
        if (action === 'callback') setCallback('');
      }
      onChange();
    } catch (cause) {
      if (action !== 'cancel' && action !== 'callback') expectedId.current = null;
      setError(cause instanceof Error ? cause.message : '登录操作失败，请重试。'); onChange();
    }
    finally { setPending(''); setCycle(value => value + 1); }
  };
  const access = new URLSearchParams(location.search); access.set('section', 'access');
  const remaining = login ? Math.max(0, Math.ceil((login.expiresAt - now) / 1000)) : 0;
  return <div className="account-login-flow">
    <p className="helper">开始登录将暂停此账户接入，完成后需重新启用。</p>
    <div className="form-actions">
      <button className="button primary" disabled={disabled || !!pending || busy || !loaded} onClick={() => void run('chatgpt', { kind: 'chatgpt' })}><LogIn size={16} aria-hidden="true" />{pending === 'chatgpt' ? '正在发起登录…' : '使用 ChatGPT 登录'}</button>
      <button className="button secondary" disabled={disabled || !!pending || busy || !loaded} onClick={() => void run('chatgptDeviceCode', { kind: 'chatgptDeviceCode' })}><KeyRound size={16} aria-hidden="true" />{pending === 'chatgptDeviceCode' ? '正在获取设备码…' : '使用设备码登录'}</button>
    </div>
    {!loaded && !connection ? <p role="status">正在读取登录状态…</p> : null}
    {connection ? <p role="status" className="helper">{connection}</p> : null}
    {error ? <p role="alert" className="field-error">{error}</p> : null}
    {login ? <div className="login-result">
      <div role="status"><h3>{titles[login.status]}</h3></div>
      {busy ? <>
        <p className="helper">{remaining > 0 ? `剩余 ${Math.floor(remaining / 60)} 分 ${remaining % 60} 秒` : '正在确认最终状态…'}。授权完成后此处会自动更新。</p>
        {login.userCode ? <p>设备码：<code>{login.userCode}</code> <button className="text-button" onClick={() => void copyText(login.userCode!).catch(() => setError('复制失败，请手动选择设备码。'))}>复制设备码</button></p> : null}
        {login.authUrl || login.verificationUrl ? <a className="button primary" href={login.authUrl ?? login.verificationUrl} target="_blank" rel="noreferrer noopener">打开授权页面<ExternalLink size={15} aria-hidden="true" /></a> : null}
        {login.authUrl || login.verificationUrl ? <button className="text-button" onClick={() => void copyText((login.authUrl ?? login.verificationUrl)!).then(() => setCopied('授权链接已复制。'), () => setError('复制失败，请右键授权页面链接复制地址。'))}>复制授权链接</button> : null}
        {copied ? <p role="status" className="account-feedback">{copied}</p> : null}
        <button className="text-button" disabled={disabled || !!pending} onClick={() => void run('cancel', { loginId: login.loginId })}>取消本次登录</button>
        {login.type === 'chatgpt' ? <details className="account-extra"><summary>已授权但没有反应？</summary>
          <p className="helper">浏览器与中转站不在同一台电脑时，本地回调可能无法到达中转站。请复制授权后地址栏中的完整回调地址，在此提交。原生登录进程必须仍在运行。</p>
          {login.manualCallbackAvailable ? <div className="account-field">
            <label htmlFor={`callback-${id}`}>完整回调地址</label>
            <textarea id={`callback-${id}`} rows={3} maxLength={16384} value={callback} onChange={event => setCallback(event.target.value)} autoComplete="off" spellCheck={false} disabled={disabled || !!pending || login.status === 'completing'} />
            <button className="button secondary" disabled={disabled || !!pending || !callback.trim() || login.status === 'completing'} onClick={() => void run('callback', { loginId: login.loginId, callbackUrl: callback.trim() })}>{login.status === 'completing' ? '已提交，等待确认' : '提交回调地址'}</button>
          </div> : <p className="helper">{login.manualCallbackReason}</p>}
        </details> : null}
      </> : <p className="helper">登录不会自动启用接入，请在“接入与调度”中确认当前接入状态。</p>}
      {login.error ? <p role="alert" className="field-error">{login.error.message}</p> : null}
      {login.warning && busy ? <p role="status" className="helper">{login.warning}</p> : null}
      {login.status === 'succeeded' ? <a className="button primary" href={`?${access}`}>前往接入与调度</a> : null}
    </div> : null}
  </div>;
}
