import { useState } from 'react';
import type { FormEvent } from 'react';
import { ThemePicker, useTheme } from './theme';

export function Connect({ onConnect, theme, serverError }: { onConnect: (token: string) => void; theme: ReturnType<typeof useTheme>; serverError: string }) {
  const [value, setValue] = useState(''); const [localError, setError] = useState('');
  const error = localError || serverError;
  const submit = (event: FormEvent) => { event.preventDefault(); if (!/^[a-f0-9]{64}$/.test(value.trim())) { setError('请粘贴本次 Rust 服务启动时生成的管理令牌。'); return; } onConnect(value.trim()); };
  return <main className="connect-screen"><div className="connect-panel"><div className="connect-heading"><img className="brand-symbol" src="/relay-icon-128.png" width={34} height={34} alt="" aria-hidden="true" /><ThemePicker {...theme} /></div><h1>连接本机工作台</h1><p>从启动终端打开完整管理链接，或在下方输入管理令牌。</p><form onSubmit={submit}><label htmlFor="admin-token">管理令牌</label><input id="admin-token" type="password" value={value} onChange={e => { setValue(e.target.value); setError(''); }} autoComplete="off" spellCheck={false} required aria-describedby={error ? 'token-error' : undefined} />{error ? <p id="token-error" className="field-error" role="alert">{error}</p> : null}<button className="button primary full" type="submit">连接工作台</button></form><p className="small-note">令牌仅用于管理本机服务，与 ChatGPT 账户凭据不同。</p></div></main>;
}
