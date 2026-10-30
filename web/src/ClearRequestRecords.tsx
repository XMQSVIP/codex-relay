import { useToast } from './Toast';
import { useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Trash2 } from 'lucide-react';
import { api } from './api';

export function ClearRequestRecords({ token, active, queued, disabled, onCleared }: {
  token: string; active: number; queued: number; disabled?: boolean; onCleared: (deleted: number) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const heading = useId();
  const description = useId();
  const [pending, setPending] = useState(false);
  const notify = useToast();
  const busy = active > 0 || queued > 0;
  const clear = async () => {
    if (pending || busy || disabled) return;
    setPending(true);
    try {
      const result = await api<{ deleted: number }>(token, '/requests', { confirmed: true }, undefined, 'DELETE');
      dialog.current?.close(); onCleared(result.deleted);
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '清空失败，请重试。', 'error');
    } finally { setPending(false); }
  };
  return <>
    <button ref={button} type="button" className="button secondary clear-records-button" disabled={disabled || pending}
      onClick={() => {  dialog.current?.showModal(); }}><Trash2 size={14} aria-hidden="true" />清空所有记录</button>
    {createPortal(<dialog ref={dialog} className="stop-service-dialog" aria-labelledby={heading} aria-describedby={description}
      onClose={() => button.current?.focus()} onCancel={event => { if (pending) event.preventDefault(); }}>
      <h2 id={heading}>清空所有请求记录？</h2>
      <p id={description}>将永久删除全部时间、全部分组的请求记录，不受当前筛选条件限制。请求图表和 Token 用量统计也会清零，此操作无法撤销。</p>
      <p>账户、API Key 和调度设置保持不变。清空后，新请求会继续记录。</p>
      {busy ? <p role="status">当前有 {active} 个执行中、{queued} 个排队请求，请先等待结束或取消。</p> : null}

      <div className="row-actions"><button type="button" className="button secondary" autoFocus disabled={pending} onClick={() => dialog.current?.close()}>取消</button>
        <button type="button" className="button danger" disabled={disabled || pending || busy} onClick={() => void clear()}>{pending ? '正在清空…' : '确认清空全部记录'}</button></div>
    </dialog>, document.body)}
  </>;
}
