import { useToast } from './Toast';
import { useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Power } from 'lucide-react';
import { api } from './api';

export function StopService({ token, disabled, accepted }: { token: string; disabled: boolean; accepted: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [busy, setBusy] = useState(false);
  const notify = useToast();
  const stop = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await api(token, '/service/stop', {});
      dialog.current?.close(); accepted();
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '无法确认停止结果，请检查服务状态。', 'error');
    } finally { setBusy(false); }
  };
  return <><button type="button" className="toolbar-icon sidebar-stop-service" title="停止中转站" aria-label="停止中转站" aria-haspopup="dialog" disabled={disabled || busy} onClick={() => { dialog.current?.showModal(); }}><Power size={18} aria-hidden="true" /></button>
    {createPortal(<dialog className="stop-service-dialog" ref={dialog} aria-labelledby="stop-service-title" aria-describedby="stop-service-help" onCancel={event => { if (busy) event.preventDefault(); }}>
      <h2 id="stop-service-title">停止整个中转站？</h2>
      <p id="stop-service-help">所有客户端入口和管理后台都将关闭。排队请求会取消，正在执行的推理会中断，并回收本服务管理的后端实例。如已启动桌面调试，其窗口会保留，但调试连接将断开。</p>
      <p>再次使用时，双击“启动中转站”即可。</p>

      <div className="row-actions"><button className="button secondary" autoFocus disabled={busy} onClick={() => dialog.current?.close()}>继续运行</button><button className="button danger" disabled={busy} onClick={() => void stop()}>{busy ? '正在提交…' : '确认停止'}</button></div>
    </dialog>, document.body)}</>;
}
