import { useToast } from './Toast';
import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import { Copy, KeyRound, LoaderCircle, Plus, Search, Pencil, Pause, Play, ChevronLeft, ChevronRight, ArrowUpDown, Trash2, X, Terminal, Zap } from 'lucide-react';
import { api } from './api';
import { copyText } from './clipboard';
import type { ClientKey, ClientKeys, CreatedClientKey, Operations } from './api';
import { KeyEditor, validKeyConcurrency } from './KeyEditor';
import { KeyFastMode } from './KeyFastMode';
import type { KeyEditValues } from './KeyEditor';
import { useQueryState, useQueryNumber } from './navigation';
import { Tabs } from './ui';
import { ClientConnection } from './ClientConnection';
import { KeyUsage } from './KeyUsage';
import { ApiKeyTransfer } from './ApiKeyTransfer';
import { keyUsageUnavailable } from './codexConfig';

type KeySortField = 'name' | 'state' | 'createdAt' | 'lastAuthenticatedAt';
const labels = { active: '已启用', paused: '已暂停', expired: '已过期', revoked: '已撤销' };
const time = (value: number | null) => value === null ? '尚未验证' : new Date(value).toLocaleString();

function KeyValue({ value }: { value: string }) {
  const suffixLength = Math.min(8, Math.floor(value.length / 2));
  return <code title={value}><span className="key-value-start">{value.slice(0, value.length - suffixLength)}</span><span className="key-value-end">{value.slice(value.length - suffixLength)}</span></code>;
}

export function ApiKeys({ token, operations, refreshVersion, onChange, allowPublicAccess }: { token: string; operations: Operations; refreshVersion: number; onChange: () => void; allowPublicAccess: boolean }) {
  const [tab] = useQueryState('tab', 'manage');
  const [data, setData] = useState<ClientKeys | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [loadError, setLoadError] = useState('');
  const notify = useToast();

  const [pending, setPending] = useState('');
  const [createOpen, setCreateOpen] = useState(false);
  const createDialog = useRef<HTMLDialogElement>(null);
  const createButton = useRef<HTMLButtonElement>(null);
  const [name, setName] = useState('');
  const [group, setGroup] = useState(() => new URLSearchParams(location.search).get('group') ?? '');
  const [models, setModels] = useState('');
  const [maxConcurrency, setMaxConcurrency] = useState(0);
  const [fastMode, setFastMode] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [filter, setFilter] = useQueryState('state', 'all');
  const [search, setSearch] = useQueryState('q', '');
  const [page, setPage] = useQueryNumber('page', 1);
  const [pageSize, setPageSize] = useQueryNumber('size', 20);
  const [editing, setEditing] = useState<ClientKey | null>(null);
  const editButton = useRef<HTMLButtonElement | null>(null);
  const [usingKeyId, setUsingKeyId] = useState<string | null>(null);
  const useButton = useRef<HTMLButtonElement | null>(null);
  const closeUsage = useCallback(() => {
    setUsingKeyId(null);
    requestAnimationFrame(() => {
      if (useButton.current?.isConnected && !useButton.current.disabled) useButton.current.focus();
      else createButton.current?.focus();
    });
  }, []);
  const [sort, setSort] = useState<{ field: KeySortField; asc: boolean }>({ field: 'createdAt', asc: false });
  const nameInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (new URLSearchParams(location.search).get('create') === '1') { setCreateOpen(true); createDialog.current?.showModal(); nameInput.current?.focus(); }
  }, []);
  useEffect(() => {
    const controller = new AbortController(); let timer: ReturnType<typeof setTimeout>;
    const load = async () => {
      try { const next = await api<ClientKeys>(token, '/api-keys', undefined, controller.signal); if (!controller.signal.aborted) { setData(next); setLoadError(''); } }
      catch (cause) { if (!controller.signal.aborted) setLoadError(cause instanceof Error ? cause.message : '密钥列表加载失败'); }
      finally { if (!controller.signal.aborted) timer = setTimeout(load, 15000); }
    };
    void load(); return () => { controller.abort(); clearTimeout(timer); };
  }, [token, refreshVersion, refresh]);
  useEffect(() => {
    if (!createOpen) return;
    const previous = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => { document.body.style.overflow = previous; };
  }, [createOpen]);
  const openCreate = () => {
    createDialog.current?.showModal();
    setCreateOpen(true);
    nameInput.current?.focus();
    if (createDialog.current) createDialog.current.scrollTop = 0;
  };
  const availableGroups = operations.groups.filter(item => item.enabled);
  const selectedGroup = group || availableGroups[0]?.id || '';
  const modelList = [...new Set(models.split(/[\s,，]+/).filter(Boolean))];
  const validModels = modelList.length <= 32 && modelList.every(model => /^[a-zA-Z0-9._:/-]{1,128}$/.test(model));
  const valid = name.trim().length > 0 && availableGroups.some(item => item.id === selectedGroup) && validModels && validKeyConcurrency(maxConcurrency);
  const busy = Boolean(pending) || operations.draining;
  const filtered = (data?.keys ?? []).filter(key => key.state !== 'revoked' && (filter === 'all' || key.state === filter) && [key.name, key.prefix, operations.groups.find(group => group.id === key.groupId)?.name ?? ''].some(value => value.toLocaleLowerCase().includes(search.toLocaleLowerCase())));
  filtered.sort((a, b) => {
    const x = a[sort.field] ?? 0, y = b[sort.field] ?? 0;
    const compared = typeof x === 'string' && typeof y === 'string' ? x.localeCompare(y, 'zh-CN') : Number(x) - Number(y);
    return (sort.asc ? compared : -compared) || a.id.localeCompare(b.id);
  });
  const sortBy = (field: typeof sort.field) => { setSort(current => ({ field, asc: current.field === field ? !current.asc : true })); setPage(1); };
  const pages = Math.max(1, Math.ceil(filtered.length / pageSize));
  const currentPage = Math.min(page, pages);
  const rows = filtered.slice((currentPage - 1) * pageSize, currentPage * pageSize);

  const create = async () => {
    if (!valid || busy) return;
    setPending('create');
    try { const result = await api<CreatedClientKey>(token, '/api-keys', { name: name.trim(), groupId: selectedGroup, models: modelList, maxConcurrency, fastMode }); createDialog.current?.close(); setData(current => current ? { ...current, keys: [{ ...result.key, secret: result.secret }, ...current.keys] } : current); notify(`“${result.key.name}”创建成功。`); setSort({ field: 'createdAt', asc: false }); setName(''); setModels(''); setGroup(''); setMaxConcurrency(0); setFastMode(false); setPage(1); setFilter('all'); setSearch(''); setRefresh(value => value + 1); }
    catch (cause) { notify(`${cause instanceof Error ? cause.message : '创建失败'} 若连接在提交后中断，请刷新列表核对。`, 'error'); }
    finally { setPending(''); }
  };
  const change = async (key: ClientKey, state: 'active' | 'paused') => {
    setPending(key.id);
    try { await api<ClientKey>(token, `/api-keys/${key.id}`, { revision: key.revision, state }); setConfirm(null); notify(`“${key.name}”${labels[state]}，下次请求即生效。`); setRefresh(value => value + 1); }
    catch (cause) { notify(cause instanceof Error ? cause.message : '操作失败', 'error'); setRefresh(value => value + 1); }
    finally { setPending(''); }
  };
  const saveKey = async (key: ClientKey, values: KeyEditValues) => {
    setPending(key.id);
    try {
      const next = await api<ClientKey>(token, `/api-keys/${key.id}`, { revision: key.revision, ...values });
      setData(current => current ? { ...current, keys: current.keys.map(item => item.id === key.id ? { ...next, secret: item.secret } : item) } : current);
      notify(`“${next.name}”的设置已保存。`); onChange();
    } catch (cause) { setRefresh(value => value + 1); throw cause; }
    finally { setPending(''); }
  };
  const remove = async (key: ClientKey) => {
    setPending(key.id);
    try {
      await api(token, `/api-keys/${key.id}`, { revision: key.revision }, undefined, 'DELETE');
      setData(current => current ? { ...current, keys: current.keys.filter(item => item.id !== key.id) } : current);
      setConfirm(null); setEditing(null); notify(`“${key.name}”已删除，历史统计保留。`); onChange();
    } catch (cause) { notify(cause instanceof Error ? cause.message : '删除失败', 'error'); }
    finally { setPending(''); }
  };
  const copy = async (text: string, label: string) => {
    try { await copyText(text); notify(`${label}已复制。`); }
    catch { notify('复制失败，请手动选择下方文本。', 'error'); }
  };
  return <div className="keys-page">
    <Tabs label="客户端接入分类" items={[{ label: 'API 密钥', href: '?view=keys', active: tab !== 'guide' }, { label: '接入指南', href: '?view=keys&tab=guide', active: tab === 'guide' }]} />
    <div hidden={tab !== 'guide'}><ClientConnection data={data} allowPublicAccess={allowPublicAccess} /></div>

    {loadError ? <div className="message error" role="alert">{loadError}</div> : null}

    <div className="keys-layout" hidden={tab === 'guide'}><section className="panel key-list" aria-labelledby="api-key-heading">
      <h2 id="api-key-heading" className="visually-hidden">API Key</h2>
      <div className="key-table-toolbar"><div className="key-search-box"><Search size={18} aria-hidden="true" /><input id="key-search" aria-label="搜索密钥名称、前缀或分组" type="search" value={search} onChange={e => { setSearch(e.target.value); setPage(1); }} placeholder="搜索名称、密钥前缀或分组" /></div><select aria-label="密钥状态" value={filter} onChange={e => { setFilter(e.target.value); setPage(1); }}><option value="all">全部状态</option>{Object.entries(labels).filter(([value]) => value !== 'revoked').map(([value,label]) => <option key={value} value={value}>{label}</option>)}</select><label className="key-mobile-sort"><span className="visually-hidden">密钥排序</span><select value={`${sort.field}:${sort.asc ? 'asc' : 'desc'}`} onChange={event => { const [field, direction] = event.target.value.split(':'); setSort({ field: field as KeySortField, asc: direction === 'asc' }); setPage(1); }}>{([['createdAt', '创建时间'], ['lastAuthenticatedAt', '最近鉴权'], ['name', '名称'], ['state', '状态']] as const).flatMap(([field, label]) => [<option key={`${field}:desc`} value={`${field}:desc`}>{label}降序</option>, <option key={`${field}:asc`} value={`${field}:asc`}>{label}升序</option>])}</select></label>
        <div className="key-toolbar-actions"><ApiKeyTransfer token={token} groups={operations.groups} disabled={busy || !data} busyChanged={value => setPending(value ? 'transfer' : '')} imported={() => { setRefresh(value => value + 1); setPage(1); setFilter('all'); setSearch(''); onChange(); }} /><button ref={createButton} className="button primary" onClick={openCreate} disabled={busy || !data}><Plus size={17} aria-hidden="true" />创建 API Key</button></div>
      </div>

      <div className={`key-table-scroll${rows.length ? '' : ' is-empty'}`} tabIndex={0} role="region" aria-label="API 密钥列表，可横向滚动"><table className="key-table"><thead><tr>
        <th scope="col" aria-sort={sort.field === 'name' ? sort.asc ? 'ascending' : 'descending' : 'none'}><button onClick={() => sortBy('name')}>名称<ArrowUpDown size={13} /></button></th><th scope="col">API Key</th>
        <th scope="col" aria-sort={sort.field === 'state' ? sort.asc ? 'ascending' : 'descending' : 'none'}><button onClick={() => sortBy('state')}>状态<ArrowUpDown size={13} /></button></th><th scope="col">分组</th><th scope="col">调用方并发</th>
        <th scope="col" aria-sort={sort.field === 'createdAt' ? sort.asc ? 'ascending' : 'descending' : 'none'}><button onClick={() => sortBy('createdAt')}>创建时间<ArrowUpDown size={13} /></button></th>
        <th scope="col" aria-sort={sort.field === 'lastAuthenticatedAt' ? sort.asc ? 'ascending' : 'descending' : 'none'}><button onClick={() => sortBy('lastAuthenticatedAt')}>最近鉴权<ArrowUpDown size={13} /></button></th><th scope="col">操作</th></tr></thead>
        <tbody>{rows.map(key => { const usageUnavailable = keyUsageUnavailable(key); return <Fragment key={key.id}><tr>
          <td data-label="名称" className="key-name-cell"><strong className="key-table-name">{key.name}{key.fastMode ? <span className="key-fast-mode-icon" role="img" aria-label="已开启强制快速模式" title="已开启强制快速模式"><Zap size={14} aria-hidden="true" /></span> : null}</strong><small title={key.models.join('、')}>{key.models.length ? key.models.join('、') : '不限制模型 ID'}</small>{key.expiresAt !== null ? <small>到期：{time(key.expiresAt)}</small> : null}</td>
          <td data-label="API Key" className="key-secret-cell"><div className="key-table-secret"><KeyValue value={key.secret ?? key.prefix} /><button className="icon-action" aria-label={`复制${key.name}完整密钥`} title={key.secret ? '复制完整密钥' : '密钥内容不可用'} disabled={!key.secret} onClick={() => key.secret && void copy(key.secret, '密钥')}><Copy size={16} aria-hidden="true" /></button></div>{!key.secret ? <small>密钥内容不可用</small> : null}</td>
          <td data-label="状态" className="key-state-cell"><span className={`key-state ${key.state}`}>{labels[key.state]}</span></td><td data-label="分组"><span className="key-group-badge">{operations.groups.find(item => item.id === key.groupId)?.name ?? '分组已删除'}</span>{operations.groups.some(item => item.id === key.groupId && !item.enabled) ? <small>分组已暂停</small> : null}</td>
          <td data-label="调用方并发"><span className="key-limit-value">{key.maxConcurrency || '∞'}</span><small>{key.maxConcurrency ? '个执行请求' : '不单独限制'}</small></td><td data-label="创建时间" className="key-table-date">{time(key.createdAt)}</td><td data-label="最近鉴权" className="key-table-date">{time(key.lastAuthenticatedAt)}</td>
          <td data-label="操作" className="key-actions-cell"><div className="key-table-actions">
            <button type="button" className="icon-action key-use-button" aria-label={`使用密钥：${key.name}`} aria-haspopup="dialog" aria-describedby={usageUnavailable ? `key-usage-reason-${key.id}` : undefined} title={usageUnavailable || '使用密钥'} disabled={busy || !!usageUnavailable} onClick={event => { useButton.current = event.currentTarget; setUsingKeyId(key.id); setConfirm(null); }}><Terminal size={16} aria-hidden="true" /></button>
            {['active','paused'].includes(key.state) ? <><button className="icon-action" title="编辑密钥" aria-label={`编辑${key.name}`} aria-haspopup="dialog" disabled={busy} onClick={event => { editButton.current = event.currentTarget; setEditing(key); setConfirm(null); }}><Pencil size={16} aria-hidden="true" /></button><button className="icon-action" title={key.state === 'active' ? '暂停密钥' : '启用密钥'} aria-label={`${key.state === 'active' ? '暂停' : '启用'}${key.name}`} disabled={busy} onClick={() => void change(key, key.state === 'active' ? 'paused' : 'active')}>{key.state === 'active' ? <Pause size={16} /> : <Play size={16} />}</button></> : null}<button className="icon-action destructive" title="删除密钥" aria-label={`删除${key.name}`} disabled={busy} onClick={() => { setConfirm(key.id); setEditing(null); }}><Trash2 size={16} aria-hidden="true" /></button></div>
            {usageUnavailable ? <small id={`key-usage-reason-${key.id}`} className="key-usage-unavailable">{usageUnavailable}</small> : null}
          </td>
        </tr>
        {confirm === key.id ? <tr className="key-expanded-row"><td colSpan={8}><div className="key-delete-confirm" role="alert"><p>删除“{key.name}”后无法恢复，使用它的客户端需要换用新密钥。历史请求统计保留。</p><div className="key-actions"><button className="button secondary" disabled={busy} onClick={() => setConfirm(null)}>保留密钥</button><button className="button danger" disabled={busy} onClick={() => void remove(key)}>确认删除</button></div></div></td></tr> : null}</Fragment>; })}
        {!rows.length ? <tr><td colSpan={8}><div className="compact-empty">{!data ? <LoaderCircle className="spin" size={22} aria-hidden="true" /> : <KeyRound size={27} aria-hidden="true" />}<p>{!data ? loadError ? '密钥列表暂时无法读取。' : '正在读取密钥…' : search || filter !== 'all' ? '没有符合筛选条件的密钥。' : '创建第一个调用方密钥，为客户端分配访问范围。'}</p>{data && (search || filter !== 'all') ? <button type="button" className="button secondary" onClick={() => { setSearch(''); setFilter('all'); setPage(1); }}>清除筛选</button> : loadError ? <button type="button" className="button secondary" onClick={() => setRefresh(value => value + 1)}>重新读取</button> : null}</div></td></tr> : null}</tbody></table></div>
      <div className="key-table-footer"><span>共 {filtered.length} 个密钥</span>{filtered.length > 10 ? <select aria-label="每页条数" value={pageSize} onChange={e => { setPageSize(Number(e.target.value)); setPage(1); }}>{[10,20,50].map(size => <option key={size} value={size}>{size} 条/页</option>)}</select> : null}{pages > 1 ? <><button className="icon-action" aria-label="上一页" disabled={currentPage <= 1} onClick={() => setPage(currentPage - 1)}><ChevronLeft size={17} /></button>{Array.from({length:Math.min(5,pages)},(_,i) => Math.min(Math.max(1,currentPage - 2), Math.max(1,pages - 4)) + i).map(number => <button key={number} className={`key-page-button ${currentPage === number ? 'selected' : ''}`} aria-current={currentPage === number ? 'page' : undefined} aria-label={`第 ${number} 页`} onClick={() => setPage(number)}>{number}</button>)}<button className="icon-action" aria-label="下一页" disabled={currentPage >= pages} onClick={() => setPage(currentPage + 1)}><ChevronRight size={17} /></button><span className="helper">{currentPage} / {pages}</span></> : null}</div>
    </section></div>
    {editing ? <KeyEditor value={editing} groups={operations.groups} disabled={busy} save={values => saveKey(editing, values)} close={() => { setEditing(null); if (editButton.current?.isConnected) editButton.current.focus(); else createButton.current?.focus(); }} /> : null}
    {usingKeyId && data ? <KeyUsage key={usingKeyId} value={data.keys.find(key => key.id === usingKeyId)} data={data} operations={operations} allowPublicAccess={allowPublicAccess} close={closeUsage} /> : null}
    <dialog className="key-create-dialog" ref={createDialog} aria-labelledby="create-key-heading" aria-describedby="create-key-description" onClose={() => { setCreateOpen(false); createButton.current?.focus(); }} onCancel={event => { if (pending === 'create') event.preventDefault(); }}>
      <div className="section-heading"><div><h2 id="create-key-heading">创建 API Key</h2><p id="create-key-description">分配分组与调用权限，新密钥长期有效。</p></div><button type="button" className="button secondary key-dialog-close" aria-label="关闭创建窗口" disabled={pending === 'create'} onClick={() => createDialog.current?.close()}><X size={18} aria-hidden="true" /></button></div>
      <form className="key-form" aria-busy={pending === 'create'} onSubmit={e => { e.preventDefault(); void create(); }}>
        <label htmlFor="key-name">密钥名称</label><input id="key-name" ref={nameInput} required maxLength={60} value={name} onChange={e => setName(e.target.value)} placeholder="例如：开发客户端" disabled={busy} />
        <label htmlFor="key-group">授权分组</label><select id="key-group" value={selectedGroup} onChange={e => setGroup(e.target.value)} disabled={busy || !availableGroups.length}>{!availableGroups.length ? <option value="">请先启用一个分组</option> : availableGroups.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select>
        <label htmlFor="key-models">允许的模型 ID（选填）</label><textarea id="key-models" value={models} onChange={e => setModels(e.target.value)} placeholder="留空不限制模型 ID，由上游判断是否支持" rows={3} maxLength={4128} aria-describedby="key-model-help" disabled={busy} /><p id="key-model-help" className="helper">留空不单独限制模型；填写时最多 32 个确切 ID，以逗号或换行分隔。</p>
        <p className="helper">新密钥长期有效，可随时暂停或删除。</p>
        <KeyFastMode value={fastMode} disabled={busy} onChange={setFastMode} />
        <label htmlFor="key-concurrency">调用方并发上限</label><input id="key-concurrency" type="number" min={0} max={256} step={1} required value={Number.isNaN(maxConcurrency) ? '' : maxConcurrency} onChange={e => setMaxConcurrency(e.target.valueAsNumber)} aria-invalid={!validKeyConcurrency(maxConcurrency)} aria-describedby="key-concurrency-help" disabled={busy} /><p id="key-concurrency-help" className="helper">0 表示不单独限制，最高 256。达到上限后进入有界等待队列；不同 Key 独立计数；实际执行仍受服务总容量和实例容量约束。</p>
        <div className="key-dialog-actions"><button type="button" className="button secondary" disabled={pending === 'create'} onClick={() => createDialog.current?.close()}>取消</button><button className="button primary" disabled={!valid || busy || !data}>{pending === 'create' ? '正在创建…' : '创建密钥'}</button></div>
      </form>
    </dialog>
  </div>;
}
