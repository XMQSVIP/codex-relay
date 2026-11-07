import { useToast } from './Toast';
import { useEffect, useId, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Globe2, KeyRound } from 'lucide-react';
import { api } from './api';
import type { AccessConfig } from './api';
import { ThemePicker, useTheme } from './theme';

export function AccessSettingsPage({ token, onSaved }: { token: string; onSaved: (config: AccessConfig) => void }) {
  const id = useId();
  const request = useRef<AbortController | null>(null);
  const saving = useRef(false);
  const [saved, setSaved] = useState<AccessConfig | null>(null);
  const [passwordEnabled, setPasswordEnabled] = useState(false);
  const [allowPublicAccess, setPublic] = useState(false);
  const [currentPassword, setCurrent] = useState('');
  const [password, setPassword] = useState('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const notify = useToast();
  const load = async () => {
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setError('');
    try {
      const config = await api<AccessConfig>(token, '/settings/access', undefined, controller.signal);
      if (!controller.signal.aborted) { setSaved(config); setPasswordEnabled(config.passwordEnabled); setPublic(config.allowPublicAccess); setCurrent(''); setPassword(''); }
    } catch (cause) { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : '设置读取失败。'); }
  };
  useEffect(() => { void load(); return () => request.current?.abort(); }, [token]);
  const save = async (event: FormEvent) => {
    event.preventDefault(); if (!saved || saving.current) return;
    setError('');
    saving.current = true; setPending(true);
    try {
      const config = await api<AccessConfig>(token, '/settings/access', { passwordEnabled, allowPublicAccess, ...(saved.passwordEnabled ? { currentPassword } : {}), ...(passwordEnabled && password ? { password } : {}) });
      setSaved(config); setCurrent(''); setPassword(''); notify('访问设置已保存。'); onSaved(config);
    } catch (cause) { notify(cause instanceof Error ? cause.message : '设置保存失败。', 'error'); }
    finally { saving.current = false; setPending(false); }
  };
  return <section className="panel security-settings"><div className="section-heading"><div><h2>访问安全</h2><p>管理后台登录与外部访问权限。</p></div></div>{saved ? <form onSubmit={event => void save(event)}>
        <fieldset disabled={pending}>
          <legend><KeyRound size={16} aria-hidden="true" />后台登录</legend>
          <div className="login-mode">
            <label><input type="radio" name={`${id}-mode`} checked={!passwordEnabled} onChange={() => setPasswordEnabled(false)} /><span>免登录<small>直接进入后台</small></span></label>
            <label><input type="radio" name={`${id}-mode`} checked={passwordEnabled} onChange={() => setPasswordEnabled(true)} /><span>密码登录<small>验证后进入后台</small></span></label>
          </div>
          {saved.passwordEnabled ? <div className="access-field"><label htmlFor={`${id}-current`}>当前密码</label><input id={`${id}-current`} type="password" autoComplete="current-password" required value={currentPassword} onChange={event => setCurrent(event.target.value)} /><p className="helper">修改访问设置时验证当前密码。</p></div> : null}
          {passwordEnabled ? <div className="access-field"><label htmlFor={`${id}-password`}>{saved.passwordEnabled ? '新密码（选填）' : '设置密码'}</label><input id={`${id}-password`} type="text" autoComplete="new-password" autoCapitalize="none" spellCheck={false} required={!saved.passwordEnabled} placeholder={saved.passwordEnabled ? '留空则保留当前密码' : '输入密码'} value={password} onChange={event => setPassword(event.target.value)} /></div> : null}
        </fieldset>
        <fieldset disabled={pending} className="public-access-fieldset"><legend><Globe2 size={16} aria-hidden="true" />外部访问</legend>
          <label className="access-switch" htmlFor={`${id}-public`}><span>允许公网用户访问<small>同时控制管理后台和客户端 API</small></span><input id={`${id}-public`} type="checkbox" role="switch" checked={allowPublicAccess} onChange={event => setPublic(event.target.checked)} /></label>
          <p className="helper">{allowPublicAccess ? '允许局域网和公网连接。公网接入仍需可达地址、端口映射或反向代理。客户端 API 始终需要 API Key。' : '关闭时，管理后台和客户端 API 仅允许本机访问。'}</p>
          {allowPublicAccess && !passwordEnabled ? <p className="access-open-note">免登录已启用，能连接此服务的外部用户可直接进入管理后台。</p> : null}
        </fieldset>
        {error ? <p id={`${id}-error`} className="field-error" role="alert">{error}</p> : null}
        <div className="access-dialog-actions"><button type="button" className="button secondary" disabled={pending} onClick={() => void load()}>还原设置</button><button type="submit" className="button primary" disabled={pending}>{pending ? '正在保存…' : '保存设置'}</button></div>
      </form> : <div className="compact-empty">{error ? <><p role="alert">{error}</p><button className="button secondary" onClick={() => void load()}>重试读取</button></> : <p role="status">正在读取访问设置…</p>}</div>}</section>;
}

export function PasswordLogin({ token, theme, onLogin }: { token: string; theme: ReturnType<typeof useTheme>; onLogin: () => void }) {
  const [password, setPassword] = useState('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const id = useId();
  const submit = async (event: FormEvent) => {
    event.preventDefault(); if (pending) return;
    setPending(true); setError('');
    try { await api(token, '/session/login', { password }); setPassword(''); onLogin(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : '登录失败，请重试。'); }
    finally { setPending(false); }
  };
  return <main className="connect-screen"><div className="connect-panel"><div className="connect-heading"><img className="brand-symbol" src="/relay-icon-128.png" width={34} height={34} alt="" aria-hidden="true" /><ThemePicker {...theme} /></div><h1>登录管理后台</h1><p>输入后台密码，继续管理账户与客户端接入。</p><form onSubmit={event => void submit(event)}><label htmlFor={id}>后台密码</label><input id={id} autoFocus type="password" autoComplete="current-password" required disabled={pending} value={password} aria-describedby={error ? `${id}-error` : undefined} onChange={event => setPassword(event.target.value)} />{error ? <p id={`${id}-error`} className="field-error" role="alert">{error}</p> : null}<button type="submit" className="button primary full" disabled={pending}>{pending ? '正在验证…' : '登录'}</button></form></div></main>;
}
