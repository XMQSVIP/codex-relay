import { useToast } from './Toast';
import { useEffect, useState } from 'react';
import { ArrowLeft, ArrowRight, Clock3, LoaderCircle, RefreshCw, X } from 'lucide-react';
import { api } from './api';
import { useQueryState, useQueryNumber } from './navigation';
import type { Operations, QueueSnapshot } from './api';

const actions: Record<string, string> = {
  responses: '文本推理', inspect_route: '读取模型路由',
  start: '启动账户', login: '发起登录', import_credentials: '导入凭据',
  account_check: '检查账户', group_check: '按组调度检查', shared: '共享连接验证',
  guard: '过滤与取消验证', stream: '流式输出映射验证', provider: '提供方验证', doctor: '桌面检查',
  prepare: '准备安装', 'inspect-provider': '读取提供方',
};

export function QueuePanel({ token, operations, refreshVersion, onChange }: {
  token: string; operations: Operations; refreshVersion: number; onChange: () => void;
}) {
  const [group, setGroup] = useQueryState('groupId', '');
  const [page, setPage] = useQueryNumber('page', 1);
  const [refresh, setRefresh] = useState(0);
  const [data, setData] = useState<QueueSnapshot | null>(null);
  const [loadError, setLoadError] = useState('');
  const notify = useToast();

  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    setData(null); setLoading(true);
    const query = new URLSearchParams({ page: String(page) });
    if (group) query.set('groupId', group);
    const load = async () => {
      try {
        const next = await api<QueueSnapshot>(token, `/queue?${query}`, undefined, controller.signal);
        if (!controller.signal.aborted) {
          const last = Math.max(1, Math.ceil(next.total / next.pageSize));
          if (page > last) setPage(last);
          setData(next); setLoadError('');
        }
      } catch (error) {
        if (!controller.signal.aborted) setLoadError(error instanceof Error ? error.message : '队列读取失败');
      } finally {
        if (!controller.signal.aborted) { setLoading(false); timer = setTimeout(load, 2000); }
      }
    };
    void load();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [token, group, page, refresh, refreshVersion]);

  const cancel = async (id: string) => {
    setPending(id);
    try {
      await api(token, `/requests/${id}/cancel`, {});
      notify('已取消等待请求，不会再派发该请求的原生操作。');
    } catch (error) {
      notify(error instanceof Error ? error.message : '取消失败', 'error');
    } finally {
      setPending(null); setRefresh(value => value + 1); onChange();
    }
  };

  return <section className="panel queue-panel" aria-labelledby="queue-panel-title">
    <div className="section-heading queue-heading">
      <div><h2 id="queue-panel-title">排队请求 <span className="count">{data?.total ?? '—'}</span></h2><p>每 2 秒更新 · 仅可取消尚未执行的请求。</p></div>
      <div className="queue-toolbar">
        <div className="queue-filter"><label className="visually-hidden" htmlFor="queue-group-filter">筛选分组</label><select id="queue-group-filter" value={group} onChange={event => { setGroup(event.target.value); setPage(1); }}><option value="">全部分组</option>{operations.groups.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></div>
        <button className="button secondary" disabled={loading} onClick={() => setRefresh(value => value + 1)}><RefreshCw size={15} aria-hidden="true" />刷新队列</button>
      </div>
    </div>
    {loadError ? <div className="message error" role="alert">{loadError}</div> : null}

    {data?.records.length ? <div className="records-scroll"><table className="records-table queue-table"><thead><tr><th scope="col">顺序 / 操作</th><th scope="col">账户 / 分组</th><th scope="col">已等待</th><th scope="col">距离超时</th><th scope="col">操作</th></tr></thead><tbody>{data.records.map(record => <tr key={record.id}>
      <td><span className="queue-position">#{record.position}</span><strong>{actions[record.action] ?? record.action}</strong><small title={record.id}>{record.id.slice(0, 8)}</small></td>
      <td>{record.accountName ?? (record.groupId ? '出队时选择账户' : '全局操作')}<small>{record.groupName ?? '—'}</small></td>
      <td>{(record.waitMs / 1000).toFixed(1)} 秒</td><td>约 {Math.ceil(record.remainingMs / 1000)} 秒</td>
      <td><button className="button secondary" disabled={pending !== null} aria-label={`取消排队请求 ${record.id.slice(0, 8)}`} onClick={() => void cancel(record.id)}>{pending === record.id ? <LoaderCircle size={15} className="spin" aria-hidden="true" /> : <X size={15} aria-hidden="true" />}取消等待</button></td>
    </tr>)}</tbody></table></div> : <div className="compact-empty"><Clock3 size={23} aria-hidden="true" /><p>{loading ? '正在读取等待请求…' : loadError ? '队列暂时无法读取，请刷新重试。' : '当前筛选范围没有排队请求。'}</p></div>}
    {data && data.total > data.pageSize ? <div className="pagination"><span>第 {page} 页 · 全局 {data?.totalQueued ?? '—'} 个等待</span><button className="button secondary" disabled={loading || page <= 1} onClick={() => setPage(value => value - 1)}><ArrowLeft size={15} aria-hidden="true" />上一页</button><button className="button secondary" disabled={loading || page * data.pageSize >= data.total} onClick={() => setPage(value => value + 1)}>下一页<ArrowRight size={15} aria-hidden="true" /></button></div> : null}
  </section>;
}
