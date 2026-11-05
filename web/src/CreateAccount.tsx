import { useToast } from './Toast';
import { useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { LoaderCircle, Plus, X } from 'lucide-react';
import { goTo } from './navigation';
import { api } from './api';
import type { Account, Operations } from './api';
import { LocalConnection } from './LocalConnection';

import { UpstreamFields, emptyUpstream, upstreamErrors } from './ApiKeyAccount';
type Mode = 'local' | 'login' | 'import' | 'api-key';
export function CreateAccount({ token, disabled, operations, accounts, onChange }: {
  token: string; disabled: boolean; operations: Operations; accounts: Account[]; onChange: () => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const prefix = useId();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<Mode>('local');
  const [note, setNote] = useState('');
  const autoName = mode === 'login' && !note.trim();
  const [upstream, setUpstream] = useState(emptyUpstream);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [pending, setPending] = useState(false);
  const notify = useToast();
  const [created, setCreated] = useState<Account | null>(null);
  useEffect(() => {
    if (created && accounts.some(account => account.id === created.id)) {
      goTo(`?view=accounts&account=${encodeURIComponent(created.id)}&section=${mode === 'api-key' ? 'access' : mode === 'import' ? 'credentials' : 'login'}`);
    }
  }, [created, accounts, mode]);
  useEffect(() => { if (!open) return; const previous = document.body.style.overflow; document.body.style.overflow = 'hidden'; return () => { document.body.style.overflow = previous; }; }, [open]);
  const create = async () => {
    if (pending || disabled || (!autoName && !note.trim())) return;
    if (mode === 'api-key' && !created) { const errors = upstreamErrors(upstream); setErrors(errors); if (Object.keys(errors).length) return; }
    setPending(true);
    try {
      if (!created) {
        const account = await api<Account>(token, '/accounts', { name: note.trim(), note: note.trim(), autoName, ...(mode === 'api-key' ? { source: 'api-key', upstream: { ...upstream, baseUrl: upstream.baseUrl.trim() } } : {}) });
        setCreated(account); setUpstream(value => ({ ...value, apiKey: '' })); notify(mode === 'api-key' ? `“${account.name}”已导入，请手动启用账户。` : `“${account.name}”已创建，请继续完成登录或导入凭据。`);
      }
      try { await onChange(); }
      catch { notify('账户已创建，但列表刷新失败。请重试刷新，不会重复创建账户。', 'error'); }
    } catch (cause) { notify(cause instanceof Error ? cause.message : '创建失败，请重试。', 'error'); }
    finally { setPending(false); }
  };
  return <>
    <button ref={trigger} type="button" className="button primary add-account-toggle" disabled={disabled} onClick={() => { setMode('local'); setNote(''); setUpstream(emptyUpstream()); setErrors({}); setCreated(null); setOpen(true); dialog.current?.showModal(); dialog.current?.querySelector<HTMLInputElement>('input[type="radio"]')?.focus(); }}><Plus size={16} aria-hidden="true" />添加账户</button>
    {createPortal(<dialog ref={dialog} className="account-create-dialog" aria-labelledby={`${prefix}-title`} onCancel={event => { if (pending) event.preventDefault(); }} onClose={() => { setOpen(false); trigger.current?.focus(); }}>
      <div className="account-dialog-heading"><h2 id={`${prefix}-title`}>添加账户</h2><button type="button" className="account-dialog-close" aria-label="关闭添加账户" disabled={pending} onClick={() => dialog.current?.close()}><X size={19} aria-hidden="true" /></button></div>
      <div className="dialog-body">
      <fieldset className="connection-methods" disabled={pending || disabled || !!created}><legend>连接方式</legend>{([['local', '使用本机 Codex'], ['login', '登录其他 ChatGPT 账户'], ['import', '导入凭据'], ['api-key', 'API Key']] as const).map(([value, label]) => <label key={value}><input type="radio" name={`${prefix}-mode`} checked={mode === value} onChange={() => { setMode(value); }} />{label}</label>)}</fieldset>
      {open && mode === 'local' ? <LocalConnection token={token} operations={operations} accounts={accounts} disabled={disabled} onChange={onChange} onBusyChange={setPending} /> : open ? <form noValidate={mode === 'api-key'} aria-busy={pending} onSubmit={event => { event.preventDefault(); void create(); }}>
        <p className="helper">{mode === 'api-key' ? '导入上游 API Key，保存后在接入与调度中启用账户。' : mode === 'login' ? '使用独立的 ChatGPT 账户，创建后继续浏览器或设备码登录。' : '创建独立账户后，继续导入凭据文件、Access Token 或 Refresh Token。'}</p>

        <div className="account-field"><label htmlFor={`${prefix}-note`}>{mode === 'login' ? '账户备注（可选）' : '账户备注'}</label><input id={`${prefix}-note`} autoComplete="off" maxLength={500} required={mode !== 'login'} placeholder={mode === 'login' ? '留空时使用登录邮箱的前缀' : undefined} value={note} disabled={pending || !!created} onChange={event => setNote(event.target.value)} /></div>
        {mode === 'api-key' ? <UpstreamFields value={upstream} change={value => { setUpstream(value); if (Object.keys(errors).length) setErrors(upstreamErrors(value)); }} errors={errors} disabled={pending || !!created} /> : null}
        <div className="account-dialog-actions"><button type="submit" className="button primary" disabled={pending || disabled || (!autoName && !note.trim())}>{pending ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : null}{pending ? created ? '正在更新账户…' : '正在创建…' : created ? '重试刷新账户' : mode === 'api-key' ? '导入 API Key 账户' : mode === 'login' ? '创建并继续登录' : '创建并导入凭据'}</button></div>
      </form> : null}
      </div>
    </dialog>, document.body)}
  </>;
}
