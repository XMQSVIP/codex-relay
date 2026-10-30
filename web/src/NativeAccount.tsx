import { useId, useState } from 'react';
import { ChevronUp, RefreshCw } from 'lucide-react';
import type { Account, Operations } from './api';
import { LocalConnection } from './LocalConnection';
import { nativeAuthLabel } from './nativeAuth';

export function NativeAccount({ token, account, accounts, operations, disabled, onChange }: {
  token: string; account: Account; accounts: Account[]; operations: Operations; disabled: boolean; onChange: () => void | Promise<void>;
}) {
  const editorId = useId();
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const policy = operations.accounts.find(item => item.id === account.id);
  const native = account.source === 'native-config';
  return <section className="native-account-details">
    <div className="native-config-heading"><h2>{native ? '使用本机 Codex 配置' : '也可以使用本机 Codex'}</h2>
      <p className="helper">{native ? '本机切换账户或提供方后，请重新检测并确认更新。' : '自动识别本机配置，在此账户中连接并启用，保留名称和分组。'}</p></div>
    {native ? <dl className="native-config-fields">
      <div><dt>提供方</dt><dd>{policy?.route?.id ?? '待检测'}</dd></div>
      <div><dt>连接方式</dt><dd>{policy?.route ? nativeAuthLabel(policy.route.auth) : '自动识别本机凭据'}</dd></div>
      <div><dt>默认模型</dt><dd>{policy?.route?.model ?? '待检测'}</dd><small>客户端可指定调用模型</small></div>
    </dl> : null}
    <div className="native-config-actions"><button type="button" className={`button ${editing ? 'secondary' : 'primary'}`} aria-expanded={editing} aria-controls={editorId} disabled={disabled || busy} onClick={() => setEditing(value => !value)}>{editing ? <ChevronUp size={16} aria-hidden="true" /> : <RefreshCw size={16} aria-hidden="true" />}{editing ? '收起检测结果' : native ? policy?.route ? '重新检测并更新' : '检测并启用' : '使用本机 Codex'}</button><span className="helper">仅检测配置，不发送模型请求</span></div>
    <div id={editorId} hidden={!editing}>{editing ? <LocalConnection token={token} target={account} accounts={accounts} operations={operations} disabled={disabled} onChange={onChange} onBusyChange={setBusy} /> : null}</div>
  </section>;
}
