import { useToast } from './Toast';
import { useRef, useState } from 'react';
import { LoaderCircle, Play, RotateCw, Square } from 'lucide-react';
import { api } from './api';
import type { Account, AccountPolicy } from './api';
import { Modal } from './ui';

type RuntimeAction = 'start' | 'stop' | 'restart';
const labels = { start: '启动', stop: '停止', restart: '重启' };

export function AccountRuntimeControls({ account, policy, token, disabled, canStart, onChange }: {
  account: Account; policy?: AccountPolicy; token: string; disabled: boolean; canStart: boolean; onChange: () => void;
}) {
  const [confirm, setConfirm] = useState<'stop' | 'restart' | null>(null);
  const [pending, setPending] = useState<RuntimeAction | null>(null);
  const inFlight = useRef(false);
  const notify = useToast();
  const live = !!account.pid || account.state === 'generating';
  const blocked = disabled || !!pending || !!policy?.deleting || account.state === 'starting';
  const startBlocked = !canStart || !!policy?.active || !!policy?.queued;
  const run = async (action: RuntimeAction) => {
    if (inFlight.current || blocked || action === 'start' && startBlocked) return;
    inFlight.current = true;
    setPending(action);
    try {
      const result = await api<{ warning?: string }>(token, `/accounts/${account.id}/${action}`, {});
      notify(result.warning ?? `实例已${labels[action]}，账户启用状态未改变。`, result.warning ? 'error' : 'success');
      setConfirm(null); onChange();
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : `${labels[action]}失败，请重试。`, 'error');
      onChange();
    } finally { inFlight.current = false; setPending(null); }
  };
  const request = (action: RuntimeAction) => {
    if (action !== 'start' && (account.state === 'generating' || policy?.active)) {
      setConfirm(action);
    } else void run(action);
  };
  if (account.source === 'api-key') return null;
  return <section className="account-extra">
    <h3>实例维护</h3><p className="helper">日常只需启用账户，后台会自动准备并维持一个可用实例。以下操作用于排查问题，不改变账户启用状态；停止或重启会中断当前任务。账户仍启用且所属分组允许调度时，停止的实例可能由后台检查或新请求自动启动。如需持续停止服务，请停用账户。</p>
    <div className="account-dialog-actions">
      <button type="button" className="button secondary" disabled={blocked || !live && startBlocked} onClick={() => request(live ? 'stop' : 'start')}>{pending && pending !== 'restart' ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : live ? <Square size={16} aria-hidden="true" /> : <Play size={16} aria-hidden="true" />}{pending && pending !== 'restart' ? `正在${labels[pending]}…` : live ? '停止实例' : '启动实例'}</button>
      <button type="button" className="button secondary" disabled={blocked || !canStart} onClick={() => request('restart')}>{pending === 'restart' ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : <RotateCw size={16} aria-hidden="true" />}{pending === 'restart' ? '正在重启…' : '重启实例'}</button>
    </div>

    {confirm ? <Modal title={`${labels[confirm]}“${account.name}”？`} busy={!!pending} close={() => setConfirm(null)}>
      <p className="helper">{labels[confirm]}会中断当前任务，账户启用状态保持不变。</p>

      <div className="account-dialog-actions"><button type="button" className="button secondary" disabled={!!pending} onClick={() => setConfirm(null)}>取消</button><button type="button" className="button danger" disabled={blocked} onClick={() => void run(confirm)}>{pending ? `正在${labels[pending]}…` : confirm === 'stop' ? '停止当前任务与实例' : '中断当前任务并重启'}</button></div>
    </Modal> : null}
  </section>;
}
