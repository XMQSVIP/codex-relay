import { GroupChoices } from './AccountGroups';
import { useEffect, useId, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { api } from './api';
import type { Account, Operations } from './api';
import { Modal } from './ui';
import { useToast } from './Toast';

type BatchResult = { action: string; status: string; items: { accountId?: string; status: string; code?: string; name?: string }[] };
const status: Record<string, string> = { succeeded: '成功', failed: '失败', skipped: '跳过' };
const actionLabels: Record<string, string> = { configure: '批量配置', delete: '批量删除' };
const reasons: Record<string, string> = { BUSY: '账号忙碌，请稍后重试', CONFIG_CONFLICT: '配置已变化，请刷新后重试', SERVICE_DRAINING: '服务正在退出', INVALID_INPUT: '配置或账号数量不符合限制', NOT_FOUND: '账号已不存在', BACKEND_START: '原生后端启动失败' };

function BatchActions({ disabled, open }: { disabled: boolean; open: (action: 'configure' | 'delete' | 'export') => void }) {
  const [expanded, setExpanded] = useState(false);
  const container = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const id = useId();
  useEffect(() => {
    if (!expanded) return;
    const dismiss = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) setExpanded(false);
    };
    document.addEventListener('pointerdown', dismiss);
    return () => document.removeEventListener('pointerdown', dismiss);
  }, [expanded]);
  return <div className="account-batch-actions" ref={container} onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget)) setExpanded(false); }} onKeyDown={event => {
    if (event.key === 'Escape') { setExpanded(false); trigger.current?.focus(); event.stopPropagation(); }
  }}>
    <button type="button" ref={trigger} className="button secondary" disabled={disabled} aria-expanded={expanded} aria-controls={id} onClick={() => setExpanded(value => !value)}>更多<ChevronDown size={15} aria-hidden="true" /></button>
    {expanded ? <div id={id} className="account-batch-options" role="group" aria-label="批量操作">
      {(['configure', 'delete', 'export'] as const).map(action => <button key={action} type="button" className="text-button" disabled={disabled} onClick={() => { setExpanded(false); trigger.current?.focus(); open(action); }}>{action === 'export' ? '导出账户备份' : actionLabels[action]}</button>)}
    </div> : null}
  </div>;
}

export function AccountBatch({ token, accounts, operations, selected, clear, reload, disabled }: { token: string; accounts: Account[]; operations: Operations; selected: string[]; clear: () => void; reload: () => Promise<void>; disabled: boolean }) {
  const notify = useToast();
  const [dialog, setDialog] = useState<'configure' | 'delete' | 'export' | null>(null);
  const [result, setResult] = useState<BatchResult | null>(null);
  const [pending, setPending] = useState(false);
  const [enabled, setEnabled] = useState('preserve'), [groupIds, setGroupIds] = useState<string[] | null>(null), [limit, setLimit] = useState('');
  const open = (next: typeof dialog) => { setDialog(next); if (next === 'configure') { setEnabled('preserve'); setGroupIds(null); setLimit(''); } };
  const close = () => setDialog(null);
  const ids = selected;
  const names = ids.map(id => accounts.find(a => a.id === id)?.name ?? id);
  const submit = async (action: 'configure' | 'delete') => {
    if (pending) return;
    setPending(true); setResult(null);
    try {
      const changes = action === 'configure' ? { ...(enabled !== 'preserve' ? { enabled: enabled === 'true' } : {}), ...(groupIds ? { groupIds } : {}), ...(limit ? { maxConcurrency: Number(limit) } : {}) } : {};
      const payload = { accountIds: ids, ...changes };
      const response = await api<BatchResult>(token, '/accounts/batch', { revision: operations.revision, action, ...payload });
      const succeeded = response.items.filter(item => item.status === 'succeeded').length;
      const failed = response.items.filter(item => item.status === 'failed').length;
      const skipped = response.items.filter(item => item.status === 'skipped').length;
      notify(`${actionLabels[action]}已完成：成功 ${succeeded}，失败 ${failed}，跳过 ${skipped}。`, failed ? 'error' : skipped ? 'info' : 'success');
      setResult({ ...response, items: response.items.map(item => ({ ...item, name: accounts.find(account => account.id === item.accountId)?.name ?? item.accountId })) });
      clear(); setDialog(null);
      try { await reload(); } catch { notify('操作已完成，但账户状态刷新失败，请刷新页面。', 'error'); }
    } catch (cause) { notify(cause instanceof Error ? cause.message : '提交失败', 'error'); }
    finally { setPending(false); }
  };
  const exportSelected = async () => {
    setPending(true); setResult(null);
    try {
      const value = await api<unknown>(token, '/accounts/export', { accountIds: ids });
      const url = URL.createObjectURL(new Blob([JSON.stringify(value, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a'); link.href = url; link.download = 'codex-relay-accounts.json'; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); close(); notify('账户备份已导出，可通过“导入账户备份”恢复。');
    } catch (cause) { notify(cause instanceof Error ? cause.message : '导出失败', 'error'); }
    finally { setPending(false); }
  };
  if (!selected.length && !dialog && !result) return null;
  return <div className="account-batch-toolbar">
    {result ? <div className="batch-result" role="status"><p>{actionLabels[result.action]}：成功 {result.items.filter(item => item.status === 'succeeded').length}，失败 {result.items.filter(item => item.status === 'failed').length}，跳过 {result.items.filter(item => item.status === 'skipped').length}</p>{result.items.some(item => item.status !== 'succeeded') ? <ul>{result.items.filter(item => item.status !== 'succeeded').map((item, index) => <li key={index}>{item.name ?? `第 ${index + 1} 项`}：{item.code ? reasons[item.code] ?? item.code : status[item.status] ?? item.status}</li>)}</ul> : null}<button className="text-button" onClick={() => setResult(null)}>关闭操作结果</button></div> : null}
    {selected.length ? <><span>已选 {selected.length} 个</span><button className="text-button" onClick={clear}>取消选择</button><BatchActions disabled={disabled || pending} open={open} /></> : null}
    {dialog ? <Modal title={dialog === 'export' ? '导出账户备份' : actionLabels[dialog]} busy={pending} close={close}>
      {dialog === 'configure' ? <form onSubmit={event => { event.preventDefault(); void submit('configure'); }}><p>选中 {ids.length} 个账号；保持原值的字段不会修改。</p><label>启用状态<select value={enabled} onChange={e => setEnabled(e.target.value)}><option value="preserve">保持原值</option><option value="true">启用</option><option value="false">停用</option></select></label><label className="checkbox-label"><input type="checkbox" checked={groupIds !== null} disabled={pending} onChange={event => setGroupIds(event.target.checked ? [operations.groups[0].id] : null)} />修改所属分组</label>{groupIds ? <><GroupChoices groups={operations.groups} value={groupIds} onChange={setGroupIds} disabled={pending} /><p className="helper">所选分组将替换这些账户的全部分组归属。</p></> : <p className="helper">所属分组：保持原值</p>}<label>账户并发上限<input type="number" min={1} max={256} value={limit} placeholder="保持原值" onChange={e => setLimit(e.target.value)} /></label><p className="helper">忙碌账号不能改分组；停用与降低并发不会打断现有请求。</p><button className="button primary" disabled={pending || enabled === 'preserve' && !groupIds && !limit}>提交配置</button></form> : null}
      {dialog === 'delete' || dialog === 'export' ? <><ul className="batch-account-names">{names.map((name, i) => <li key={ids[i]}>{name}</li>)}</ul><p>{dialog === 'delete' ? '删除空闲账号的本地文件、登录信息与路由，保留历史统计。正在执行或排队的账号将跳过。' : '下载的账户备份包含名称、分组、并发配置、独立账户凭据和 API Key 账户的上游密钥，可通过“导入账户备份”完整恢复，请妥善保管。本机配置和桌面绑定只导出配置，导入后需要重新连接。忙碌账号请稍后导出。'}</p><button className={`button ${dialog === 'delete' ? 'danger' : 'primary'}`} disabled={pending || !ids.length} onClick={() => void (dialog === 'delete' ? submit('delete') : exportSelected())}>{dialog === 'delete' ? '确认删除空闲账号' : '确认下载账户备份'}</button></> : null}
    </Modal> : null}
  </div>;
}
