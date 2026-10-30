import { useToast } from './Toast';
import { useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Trash2 } from 'lucide-react';
import { api } from './api';
import type { AccountPolicy } from './api';

export function DeleteAccount({ token, account, revision, disabled, iconOnly = false, onChange }: {
  token: string; account: AccountPolicy; revision: number; disabled?: boolean; iconOnly?: boolean; onChange: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const heading = useId();
  const [pending, setPending] = useState(false);
  const notify = useToast();
  const busy = account.active > 0 || account.queued > 0;
  const remove = async () => {
    if (pending) return;
    setPending(true);
    try {
      await api(token, `/accounts/${account.id}`, { revision }, undefined, 'DELETE');
      dialog.current?.close(); notify(`“${account.name}”已删除，历史统计保留。`); onChange();
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '删除失败', 'error'); onChange();
    } finally { setPending(false); }
  };
  if (account.id === '00000000-0000-0000-0000-000000000001') return <span className="helper">桌面连接仅用于本机调试，请在“系统设置 → 环境与诊断”管理。</span>;
  return <>
    <button ref={button} type="button" className={iconOnly ? 'account-icon-button account-delete-button' : 'button secondary'} disabled={disabled || pending}
      aria-label={`删除账户 ${account.name} ${account.id.slice(0, 8)}`}
      title={account.deleting ? '重试删除账户' : '删除账户'}
      onClick={() => {  dialog.current?.showModal(); }}>
      <Trash2 size={iconOnly ? 18 : 15} aria-hidden="true" />{!iconOnly ? account.deleting ? '重试删除' : '删除账户' : null}
    </button>
    {createPortal(<dialog ref={dialog} className="stop-service-dialog" aria-labelledby={heading}
      onClose={() => button.current?.focus()} onCancel={event => { if (pending) event.preventDefault(); }}>
      <h2 id={heading}>删除“{account.name}”？</h2>
      <p>实例 ID：<code>{account.id.slice(0, 8)}</code></p>
      <p>将停止此实例并删除它的本地文件、独立登录信息和模型路由。历史统计保留，桌面客户端的登录和配置不受影响。</p>
      {account.route ? <p>此账户已启用接入。删除后，同组密钥需要其他可用账户才能继续转发请求。</p> : null}
      {busy ? <p role="status">此账户还有 {account.active} 个执行中、{account.queued} 个排队请求，请先等待结束或取消。</p> : null}

      <div className="row-actions"><button type="button" className="button secondary" autoFocus disabled={pending} onClick={() => dialog.current?.close()}>取消</button>
        <button type="button" className="button danger" disabled={pending || busy || disabled} onClick={() => void remove()}>{pending ? '正在删除…' : '确认删除'}</button></div>
    </dialog>, document.body)}
  </>;
}
