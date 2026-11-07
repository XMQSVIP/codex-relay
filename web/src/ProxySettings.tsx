import { useEffect, useId, useState } from 'react';
import { Globe, Save } from 'lucide-react';
import type { AccountProxy, ProxyStatus } from './api';

export const proxyModeLabel = { auto: '自动', direct: '直连', custom: '指定代理' };
export const proxyStatusLabel = { applied: '已生效', switching: '切换中', failed: '切换失败' };

export function ProxySettings({ value, status, revision, disabled, save }: {
  value: AccountProxy;
  status?: ProxyStatus;
  revision: number;
  disabled: boolean;
  save: (config: AccountProxy, revision: number) => Promise<{ config: AccountProxy; revision: number } | null>;
}) {
  const id = useId();
  const [draft, setDraft] = useState({ ...value, revision });
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => { if (!editing) setDraft({ mode: value.mode, url: value.url, revision }); }, [value.mode, value.url, revision, editing]);
  const locked = disabled || saving;
  const dirty = draft.mode !== value.mode || (draft.mode === 'custom' && draft.url.trim() !== value.url);
  const update = (config: Partial<AccountProxy>) => { setEditing(true); setDraft(current => ({ ...current, ...config })); setError(''); };
  const persist = async (config: AccountProxy, baseRevision: number, preserveDraft = false) => {
    if (locked) return;
    setSaving(true);
    try {
      const result = await save(config, baseRevision);
      if (result) {
        if (preserveDraft) setDraft(current => ({ ...current, revision: result.revision }));
        else { setDraft({ ...result.config, revision: result.revision }); setEditing(false); }
      }
    } finally { setSaving(false); }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (locked || !dirty) return;
    setError('');
    if (draft.mode === 'custom') {
      try {
        const raw = draft.url.trim();
        const parsed = new URL(raw);
        if (!['http:', 'https:'].includes(parsed.protocol) || !parsed.hostname || parsed.username || parsed.password || !['', '/'].includes(parsed.pathname) || parsed.search || parsed.hash || parsed.port === '0' || /[\x00-\x1f\x7f]/.test(raw)) throw new Error();
      } catch {
        setError('请输入有效的 HTTP/HTTPS 代理地址，不包含账号密码、路径或查询参数。');
        return;
      }
    }
    await persist({ mode: draft.mode, url: draft.mode === 'custom' ? draft.url.trim() : '' }, draft.revision);
  };
  return <div className="proxy-settings">
    <div className="section-heading"><h2><Globe size={17} aria-hidden="true" />账户代理</h2><span className="settings-state" role="status" aria-live="polite">{status?.state === 'switching' ? '正在立即重启…' : proxyStatusLabel[status?.state ?? 'applied']}</span></div>
    {status?.state === 'failed' ? <div role="alert"><p className="field-error">{status.error?.message ?? '代理切换失败。'} 配置已保存，此账户暂不接收新任务。</p><button type="button" className="button secondary" disabled={locked} onClick={() => void persist(value, revision, editing)}>重试切换</button></div> : null}
    <form aria-label="账户代理设置" onSubmit={event => void submit(event)}>
      <div className="proxy-fields">
        <div><label htmlFor={`${id}-mode`}>连接方式</label><select id={`${id}-mode`} value={draft.mode} disabled={locked} onChange={event => update({ mode: event.target.value as AccountProxy['mode'] })}>{Object.entries(proxyModeLabel).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select></div>
        {draft.mode === 'custom' ? <div className="proxy-url"><label htmlFor={`${id}-url`}>代理地址</label><input id={`${id}-url`} type="url" value={draft.url} required maxLength={2048} placeholder="http://127.0.0.1:7897" autoComplete="off" spellCheck={false} disabled={locked} aria-invalid={!!error} aria-describedby={`${id}-help${error ? ` ${id}-error` : ''}`} onChange={event => update({ url: event.target.value })} /></div> : <p className="proxy-mode-help">{draft.mode === 'auto' ? '优先使用启动环境的代理变量；未指定时读取 Windows / macOS 系统手动 HTTP/HTTPS 代理。系统代理变更后，请重启已运行的账户实例。' : '此账户直接联网，忽略代理环境变量和系统代理。'}</p>}
      </div>
      {error ? <p id={`${id}-error`} className="field-error" role="alert">{error}</p> : null}
      <div className="settings-card-footer">
        <p id={`${id}-help`} className="proxy-help">仅影响此账户的登录、请求和检查，本机通信始终直连。保存会重启运行中的实例，中断当前请求和登录；已停止的实例保持停止。</p>
        <div className="settings-card-actions">
          {editing ? <button type="button" className="text-button" disabled={locked} onClick={() => { setDraft({ ...value, revision }); setEditing(false); setError(''); }}>还原代理</button> : null}
          <button className="button secondary" disabled={locked || !dirty || (draft.mode === 'custom' && !draft.url.trim())}><Save size={15} aria-hidden="true" />{saving ? '正在保存…' : '保存代理'}</button>
        </div>
      </div>
    </form>
  </div>;
}
