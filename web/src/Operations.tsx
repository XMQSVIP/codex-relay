import { accountGroupIds } from './accountMembership';
import { useToast } from './Toast';
import { useId, useRef, useState } from 'react';
import { ArrowRight, ChevronDown, Folder, FolderPlus, LoaderCircle, Play, Save, Search, Trash2, X } from 'lucide-react';
import { api, ApiError } from './api';
import { Modal } from './ui';
import { useQueryState } from './navigation';
import type { Group, Operations } from './api';

type Mutate = (path: string, body: unknown, method?: string, message?: string) => Promise<boolean>;

export function Groups({ token, operations: ops, reload }: { token: string; operations: Operations; reload: () => void }) {
  const [query, setQuery] = useQueryState('q', '');
  const [state, setState] = useQueryState('state', 'all');
  const [checking, setChecking] = useState<string | null>(null);
  const editorTrigger = useRef<HTMLButtonElement | null>(null);
  const closeEditor = () => { setEditing(null); editorTrigger.current?.focus(); };
  const [pending, setPending] = useState(false); const notify = useToast();
  const [editing, setEditing] = useState<Group | 'new' | null>(null); const [remove, setRemove] = useState<Group | null>(null);
  const mutate: Mutate = async (path, body, method, message = '已保存，后续请求按新设置执行。') => {
    setPending(true);
    try { await api(token, path, body, undefined, method); reload(); notify(message); return true; }
    catch (e) { notify(e instanceof Error ? e.message : '操作失败', 'error'); return false; } finally { setPending(false); }
  };
  const check = async (group: Group) => {
    setChecking(group.id); setPending(true);
    try { const result = await api<{ accountId: string; credentialsLoaded: boolean }>(token, `/groups/${group.id}/check`, {}); notify(`已调度到“${ops.accounts.find(a => a.id === result.accountId)?.name ?? result.accountId}”，${result.credentialsLoaded ? '凭据已加载' : '尚未登录'}。未发送模型请求。`); reload(); }
    catch (e) { if (e instanceof ApiError && e.code === 'REQUEST_CANCELLED') notify('等待请求已取消。'); else notify(e instanceof Error ? e.message : '检查失败', 'error'); reload(); } finally { setPending(false); setChecking(null); }
  };
  const membersByGroup = new Map(ops.groups.map(group => [group.id, ops.accounts.filter(account => accountGroupIds(account).includes(group.id))]));
  const filtered = ops.groups.filter(group => {
    const text = [group.name, ...(membersByGroup.get(group.id) ?? []).map(account => account.name)].join(' ').toLocaleLowerCase();
    return text.includes(query.trim().toLocaleLowerCase()) && (state === 'all' || state === 'enabled' && group.enabled || state === 'paused' && !group.enabled);
  });
  return <div className="ops-layout groups-layout">
    {editing ? <Modal title={editing === 'new' ? '新建分组' : '编辑分组'} busy={pending} close={closeEditor}><GroupEditor group={editing === 'new' ? undefined : editing} revision={ops.revision} disabled={pending} cancel={closeEditor} save={async values => { if (await mutate(editing === 'new' ? '/groups' : `/groups/${editing.id}`, values)) closeEditor(); }} /></Modal> : null}

    {checking ? <div className="message info" role="status">正在等待或执行原生检查，全局当前有 {ops.queued} 个请求排队。等待上限为入队时的配置值，超时后会自动返回结果。</div> : null}

    <section className="panel groups-panel" aria-labelledby="groups-title">
      <h2 id="groups-title" className="visually-hidden">账户分组</h2>
      <div className="group-toolbar"><label className="inventory-search"><Search size={17} aria-hidden="true" /><span className="visually-hidden">搜索分组或组内账户</span><input type="search" value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索分组或组内账户" /></label><div className="inventory-filters" role="group" aria-label="筛选分组状态">{[['all', '全部'], ['enabled', '已启用'], ['paused', '已暂停']].map(([value, label]) => <button key={value} type="button" aria-pressed={state === value} onClick={() => setState(value)}>{label}<span>{ops.groups.filter(group => value === 'all' || value === 'enabled' && group.enabled || value === 'paused' && !group.enabled).length}</span></button>)}</div><span className="inventory-result" role="status" aria-atomic="true">显示 {filtered.length} 个分组</span>
        <button className="button primary" aria-expanded={editing === 'new'} disabled={pending} onClick={event => { editorTrigger.current = event.currentTarget; setEditing('new'); setRemove(null); }}><FolderPlus size={16} aria-hidden="true" />新建分组</button>
      </div>

      <div className="group-rows">{filtered.map(group => <article className="group-row" key={group.id} aria-label={group.name} aria-busy={checking === group.id}>
        <div className="group-identity"><span className="group-folder"><Folder size={21} aria-hidden="true" /></span><div><div className="group-name"><h3>{group.name}</h3><span className={`badge ${group.enabled ? 'success' : 'neutral'}`}>{group.enabled ? '已启用' : '已暂停'}</span></div><span>{group.memberCount} 个账户，{(membersByGroup.get(group.id) ?? []).filter(account => group.enabled && account.enabled && account.route).length} 个接入可用</span></div></div>
        <div className="group-activity"><span><strong>{group.active}</strong> 执行中</span><span><strong>{group.queued}</strong> 排队</span></div>
        <div className="row-actions">
          <button className="button secondary" disabled={pending || !group.enabled || !group.memberCount} onClick={() => void check(group)}>{checking === group.id ? <LoaderCircle size={14} className="spin" aria-hidden="true" /> : <Play size={14} aria-hidden="true" />}{checking === group.id ? '检查中…' : '调度检查'}</button>
          <button className="button secondary" aria-label={`编辑分组：${group.name}`} aria-expanded={editing !== null && editing !== 'new' && editing.id === group.id} disabled={pending} onClick={event => { editorTrigger.current = event.currentTarget; setEditing(group); setRemove(null); }}>编辑</button>
          <button className="icon-action" aria-label={`删除${group.name}`} title={group.id === '00000000-0000-0000-0000-000000000000' ? '默认分组不能删除' : group.memberCount ? '请先移走账户' : '删除空分组'} disabled={pending || !!group.memberCount || !!group.active || !!group.queued || group.id === '00000000-0000-0000-0000-000000000000'} onClick={() => { setRemove(group); setEditing(null); }}><Trash2 size={16} aria-hidden="true" /></button>
        </div>
        <details className="group-members"><summary><ChevronDown size={15} aria-hidden="true" /><span>组内账户</span><span className="group-member-preview">{(membersByGroup.get(group.id) ?? []).map(account => account.name).join('、') || '暂未分配账户'}</span><span className="count">{group.memberCount}</span></summary><div className="group-members-heading"><span>账户与接入状态</span><a className="text-button" href="?view=accounts">调整归属<ArrowRight size={13} aria-hidden="true" /></a></div>{membersByGroup.get(group.id)?.length ? <ul>{(membersByGroup.get(group.id) ?? []).map(account => <li key={account.id}><a href={`?view=accounts&account=${encodeURIComponent(account.id)}`}>{account.name}</a><span className={`badge ${group.enabled && account.enabled && account.route ? 'success' : 'neutral'}`}>{!group.enabled ? '分组已暂停' : !account.enabled ? '已暂停调度' : account.route ? '已配置接入' : '未配置接入'}</span><span className="group-member-capacity">{account.active} / {account.maxConcurrency} 并发</span></li>)}</ul> : <p>还没有账户。<a className="text-button" href="?view=accounts">前往账户页分配</a></p>}</details>

        {remove?.id === group.id ? <div className="inline-confirm" role="alert"><span>删除空分组“{remove.name}”？历史统计会保留。</span><button className="button secondary" disabled={pending} onClick={() => setRemove(null)}>保留</button><button className="button danger" disabled={pending} onClick={async () => { if (await mutate(`/groups/${remove.id}`, { revision: ops.revision }, 'DELETE')) setRemove(null); }}>删除分组</button></div> : null}
      </article>)}</div>
      {!filtered.length ? <div className="inventory-empty"><Search size={28} aria-hidden="true" /><h3>没有匹配的分组</h3><p>试试其他分组名或账户名，或清除状态筛选。</p><button className="button secondary" onClick={() => { setQuery(''); setState('all'); }}>清除筛选</button></div> : null}
      <p className="panel-foot">调度检查会启动原生实例并读取登录状态，不调用模型。可在账户页停止实例。</p>
    </section>

  </div>;
}
function GroupEditor({ group, revision, disabled, save, cancel }: { group?: Group; revision: number; disabled: boolean; save: (value: { revision: number; name: string; enabled: boolean }) => Promise<void>; cancel: () => void }) {
  const [baseRevision] = useState(revision);
  const [name, setName] = useState(group?.name ?? ''); const [enabled, setEnabled] = useState(group?.enabled ?? true); const id = useId();
  return <form className="group-editor" onSubmit={e => { e.preventDefault(); void save({ revision: baseRevision, name, enabled }); }}><div><label htmlFor={`${id}-name`}>分组名称</label><input id={`${id}-name`} value={name} maxLength={60} required autoFocus onChange={e => setName(e.target.value)} disabled={disabled} placeholder="例如：日常使用" /></div><label className="checkbox-label"><input type="checkbox" checked={enabled} onChange={e => setEnabled(e.target.checked)} disabled={disabled} />启用分组</label><div className="row-actions"><button className="button primary" disabled={disabled || !name.trim()}><Save size={16} aria-hidden="true" />{group ? '保存分组' : '创建分组'}</button><button type="button" className="button secondary" disabled={disabled} onClick={cancel}><X size={16} aria-hidden="true" />取消</button></div></form>;
}
