import { useToast } from './Toast';
import { useId, useState } from 'react';
import { ChevronDown, KeyRound, SlidersHorizontal } from 'lucide-react';
import { api } from './api';
import type { AccountPolicy } from './api';
import { AccountActivationSwitch } from './AccountActivationSwitch';
import type { ReactNode } from 'react';

export function AccountAccess({ apiKey = false, id, token, policy, groupEnabled, keyGroupId, revision, disabled, canEnable, activationHelp, onChange }: {
  apiKey?: boolean; id: string; token: string; policy?: AccountPolicy; groupEnabled: boolean; keyGroupId?: string; revision: number; disabled: boolean; canEnable: boolean; onChange: () => void;
  activationHelp?: ReactNode;
}) {
  const prefix = useId();
  const [model, setModel] = useState('');
  const [pending, setPending] = useState('');
  const notify = useToast();
  const busy = disabled || !!pending;
  const diagnosticBusy = busy || !!policy?.active || !!policy?.queued;
  const enabled = !!policy?.route && policy.enabled;
  const activationPending = pending === 'enable' || pending === 'disable' ? pending : null;
  const routeStatus = activationPending === 'enable' ? '启用中' : activationPending === 'disable' ? '停用中' : !enabled ? policy?.active ? '停用中' : '已停用' : !groupEnabled ? '分组已暂停' : policy?.health?.state === 'cooling' ? '冷却中' : policy?.health?.state === 'halfOpen' ? '待恢复' : '已启用';
  const run = async (name: string, path: string, body: unknown, success: string) => {
    setPending(name);
    try {
      const result = await api<{ warning?: string }>(token, `/accounts/${id}/${path}`, body);
      notify(result.warning ?? success, result.warning ? 'error' : 'success'); onChange();
    } catch (error) {
      notify(error instanceof Error ? error.message : '操作失败', 'error'); onChange();
    } finally { setPending(''); }
  };
  return <section className="account-group" aria-labelledby={`${prefix}-heading`}>
    <div className="account-group-title-row"><h2 id={`${prefix}-heading`}><SlidersHorizontal size={17} aria-hidden="true" />账户启用</h2><span className="settings-state">{routeStatus}</span></div>
    <div className="account-group-body">
      <div className="account-access-status"><strong>{activationPending === 'enable' ? '正在启用账户，请稍候' : activationPending === 'disable' ? '正在停用账户，请稍候' : enabled ? apiKey ? '账户已启用，直接连接上游' : '账户已启用，自动启动一个实例' : policy?.active ? '账户正在停用，等待当前任务完成' : '账户未启用，不接收新请求'}</strong><p className="helper" id={`${prefix}-activation-help`}>{apiKey ? '开启后直接向上游发送请求；关闭后不接收新请求，当前任务继续完成。上游配置会保留。' : '开启后接收客户端请求，自动启动一个实例；关闭后不接收新请求，当前任务完成后停止实例。配置和登录凭据会保留。'}</p></div>
      <div className="account-access-actions">
        <AccountActivationSwitch name={policy?.name ?? id} enabled={enabled} pending={activationPending} disabled={busy || !policy || !enabled && !canEnable} describedBy={`${prefix}-activation-help`} onChange={value => void run(value ? 'enable' : 'disable', 'activation', { revision, enabled: value }, value ? apiKey ? '账户已启用。' : '账户已启用，正在自动启动实例。' : '账户已停用，当前任务完成后停止实例。')} />
        {policy?.route ? <><button type="button" className="button secondary" disabled={diagnosticBusy} onClick={() => void run('refresh', 'route', { revision, enabled: true }, '接入信息已刷新，账户启用状态未改变。')}>{pending === 'refresh' ? '正在读取配置…' : '刷新接入信息'}</button><a className="button secondary" href={`?view=keys&create=1&group=${encodeURIComponent(keyGroupId ?? policy.groupIds[0])}`}><KeyRound size={16} aria-hidden="true" />创建 API Key</a></> : null}
      </div>
      {!enabled ? activationHelp ?? (!canEnable ? <p className="helper">{apiKey ? '请先保存有效的上游配置。' : '请先完成登录或导入凭据。'}</p> : null) : null}
      {enabled && !groupEnabled ? <p className="helper">所有所属分组均已暂停，恢复任一分组后即可接收该组的新请求。</p> : null}
      <p className="helper">启用不发送模型请求，模型由客户端选择。{policy?.route ? '同分组已有 API Key 可继续使用。' : ''}</p>

      <details className="account-extra account-connection-test"><summary><ChevronDown size={15} aria-hidden="true" />测试连接（可选）</summary><div className="account-diagnostic-body">
        <div className="account-field"><label htmlFor={`${prefix}-model`}>{apiKey ? '测试模型' : '测试模型（选填）'}</label><input id={`${prefix}-model`} value={model} onChange={event => setModel(event.target.value)} maxLength={128} placeholder={apiKey ? '输入上游模型 ID' : policy?.route?.model ?? '原生默认模型'} disabled={diagnosticBusy} aria-describedby={`${prefix}-model-help`} /><p id={`${prefix}-model-help`} className="helper">{apiKey ? '必填，仅用于这次测试，不限制客户端选择。' : '仅用于这次测试，不限制客户端选择。留空使用原生默认模型。'}</p></div>
        <p className="helper account-usage-note">测试会发送一次真实模型请求并消耗账户用量，不改变接入状态。</p>
        <button type="button" className="button secondary" disabled={diagnosticBusy || !policy || apiKey && !model.trim()} onClick={() => void run('test', 'connection-test', { ...(model.trim() ? { model: model.trim() } : {}) }, '连接测试通过，账户启用状态未改变。')}>{pending === 'test' ? '正在测试连接…' : '测试连接'}</button>
      </div></details>
    </div>
  </section>;
}
