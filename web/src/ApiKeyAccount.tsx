import { useEffect, useId, useState } from 'react';
import { api } from './api';
import type { Account } from './api';
import { useToast } from './Toast';

export type UpstreamForm = { baseUrl: string; apiKey: string; transport: 'http-sse' | 'ws-first' };
export const emptyUpstream = (): UpstreamForm => ({ baseUrl: '', apiKey: '', transport: 'http-sse' });
export function upstreamErrors(value: UpstreamForm, editing = false): Record<string, string> {
  const errors: Record<string, string> = {};
  try {
    const url = new URL(value.baseUrl.trim());
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error();
  } catch { errors.baseUrl = '请输入完整的 HTTP/HTTPS API 前缀，不包含账号密码、查询参数或片段。'; }
  if ((!editing && !value.apiKey.trim()) || value.apiKey && (!value.apiKey.trim() || /[\r\n\x00-\x1f\x7f]/.test(value.apiKey))) errors.apiKey = '请输入有效的 API 密钥。';
  return errors;
}
export function UpstreamFields({ value, change, errors, disabled, editing = false }: { value: UpstreamForm; change: (value: UpstreamForm) => void; errors: Record<string, string>; disabled: boolean; editing?: boolean }) {
  const id = useId();
  const [show, setShow] = useState(false);
  return <>
    <div className="account-field"><label htmlFor={`${id}-url`}>基础 URL <span aria-hidden="true">*</span></label><input id={`${id}-url`} type="url" value={value.baseUrl} onChange={event => change({ ...value, baseUrl: event.target.value })} placeholder="https://api.example.com/v1" autoComplete="off" maxLength={2048} disabled={disabled} aria-invalid={!!errors.baseUrl} aria-describedby={`${id}-url-help`} /><p id={`${id}-url-help`} className={errors.baseUrl ? 'message error' : 'helper'}>{errors.baseUrl || '填写上游 API 前缀，保留所需的 /v1 等路径。'}</p></div>
    <div className="account-field"><label htmlFor={`${id}-key`}>API 密钥{editing ? '（留空保留）' : ' *'}</label><input id={`${id}-key`} type={show ? 'text' : 'password'} value={value.apiKey} onChange={event => change({ ...value, apiKey: event.target.value })} placeholder={editing ? '已配置密钥；填写新值以替换' : '输入上游密钥'} autoComplete="new-password" spellCheck={false} maxLength={8192} disabled={disabled} aria-invalid={!!errors.apiKey} aria-describedby={errors.apiKey ? `${id}-key-error` : undefined} /><button type="button" className="text-button" onClick={() => setShow(!show)} disabled={disabled} aria-pressed={show}>{show ? '隐藏密钥' : '显示密钥'}</button>{errors.apiKey ? <p className="message error" id={`${id}-key-error`}>{errors.apiKey}</p> : null}</div>
    <fieldset className="connection-methods" disabled={disabled}><legend>传输方式</legend>{([['http-sse', 'HTTP / SSE'], ['ws-first', 'WS 优先']] as const).map(([mode, label]) => <label key={mode}><input type="radio" name={`${id}-transport`} checked={value.transport === mode} onChange={() => change({ ...value, transport: mode })} />{label}</label>)}</fieldset>
    <p className="helper">WS 优先适用于 Responses；上游明确不支持 WS 时回退 HTTP。图片接口使用 HTTP。</p>
  </>;
}
export function ApiKeyAccount({ account, token, revision, disabled, onChange }: { account: Account; token: string; revision: number; disabled: boolean; onChange: () => void }) {
  const configured = !!account.upstream?.hasApiKey;
  const [value, setValue] = useState<UpstreamForm>({ baseUrl: account.upstream?.baseUrl ?? '', apiKey: '', transport: account.upstream?.transport ?? 'http-sse' });
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [pending, setPending] = useState(false);
  const notify = useToast();
  const [draftRevision, setDraftRevision] = useState<number | null>(null);
  useEffect(() => {
    if (draftRevision === null && !pending) setValue({ baseUrl: account.upstream?.baseUrl ?? '', apiKey: '', transport: account.upstream?.transport ?? 'http-sse' });
  }, [account.upstream?.baseUrl, account.upstream?.transport, draftRevision, pending]);
  const save = async () => {
    const errors = upstreamErrors(value, configured); setErrors(errors);
    if (Object.keys(errors).length || disabled || pending) return;
    setPending(true);
    try {
      await api(token, `/accounts/${account.id}/upstream`, { revision: draftRevision ?? revision, baseUrl: value.baseUrl.trim(), transport: value.transport, ...(value.apiKey ? { apiKey: value.apiKey } : {}) });
      setValue({ ...value, apiKey: '' }); setDraftRevision(null); notify('上游配置已保存。'); onChange();
    } catch (cause) { notify(cause instanceof Error ? cause.message : '保存失败', 'error'); }
    finally { setPending(false); }
  };
  return <form noValidate onSubmit={event => { event.preventDefault(); void save(); }} aria-busy={pending}><div className="section-heading"><h2>API Key 上游配置</h2></div><UpstreamFields value={value} change={value => { setDraftRevision(old => old ?? revision); setValue(value); if (Object.keys(errors).length) setErrors(upstreamErrors(value, configured)); }} errors={errors} disabled={disabled || pending} editing={configured} /><p className="helper">{configured ? '保存相同配置不会中断续聊。更换上游身份后需重新开始会话。' : '请填写上游 API 密钥，保存后再启用账户。'}保存不会发送模型请求。</p><div className="account-dialog-actions"><button type="submit" className="button primary" disabled={disabled || pending}>{pending ? '正在保存…' : '保存上游配置'}</button></div></form>;
}
