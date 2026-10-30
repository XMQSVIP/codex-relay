import { useEffect, useId, useRef } from 'react';
import { ArrowDown, ArrowUp, Bot, CornerDownRight, Database, Eye, X, Zap } from 'lucide-react';
import type { RequestRecord } from './api';
import { TokenCount } from './TokenCount';
import { exactTokens } from './tokenFormat';
import { RequestFailureContent } from './RequestFailureContent';

export const requestKinds: Record<string, string> = { management: '账户操作', diagnostic: '诊断', native_check: '原生调度检查', inference: '模型推理' };
export const requestStatuses: Record<string, string> = { queued: '排队中', timed_out: '等待超时', running: '进行中', succeeded: '成功', failed: '失败', rejected: '已拒绝', cancelled: '已取消', interrupted: '服务中断' };
const actions: Record<string, string> = { chat_test: '账户测试对话', connection_test: '连接测试', responses: '客户端推理', 'web.search': '网页搜索', 'api.rejected': '接口拒绝', 'images.generations': '图片生成', 'images.edits': '图片编辑', inspect_route: '读取模型路由', start: '启动账户', stop: '停止账户', login: '发起登录', cancel_login: '取消登录', import_credentials: '导入凭据', account_check: '检查账户', group_check: '按组调度检查', shared: '桌面连接协议验证', guard: '过滤与取消验证', stream: '流式输出映射验证', provider: '提供方验证', doctor: '桌面检查', prepare: '准备安装', 'inspect-provider': '读取提供方' };
export const formatDuration = (ms: number | null | undefined) => ms == null ? '—' : ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toLocaleString('zh-CN', { minimumFractionDigits: 0, maximumFractionDigits: 2 })} s`;
const cacheRate = (r: RequestRecord) => r.cachedInputTokens == null || !r.inputTokens ? '未知' : `${(r.cachedInputTokens / r.inputTokens * 100).toLocaleString('zh-CN', { maximumFractionDigits: 1 })}%`;
const date = (ms: number) => new Date(ms).toLocaleString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const action = (r: RequestRecord) => actions[r.action] ?? r.action;
const effort = (r: RequestRecord) => r.reasoningEffort === 'default' ? '默认' : r.reasoningEffort ?? '—';
const serviceModes: Record<string, string> = { priority: '快速模式', fast: '快速模式', ultrafast: '超快速模式', default: '普速', auto: '自动', flex: 'Flex', scale: 'Scale' };
const isFastTier = (tier: string | null | undefined) => tier === 'priority' || tier === 'fast' || tier === 'ultrafast';
const confirmedStandardTier = (tier: string | null | undefined) => tier === 'default' || tier === 'flex' || tier === 'scale';
const serviceMode = (tier: string | null | undefined) => tier ? serviceModes[tier] ?? tier : '自动';
function RequestFastMode({ record: r }: { record: RequestRecord }) {
  if (r.kind !== 'inference' || (!isFastTier(r.serviceTier) && !isFastTier(r.responseServiceTier))) return null;
  const confirmed = isFastTier(r.responseServiceTier);
  const downgraded = !confirmed && confirmedStandardTier(r.responseServiceTier);
  const state = confirmed ? 'confirmed' : downgraded ? 'downgraded' : 'unconfirmed';
  const label = confirmed ? `${serviceMode(r.responseServiceTier)}（上游已确认）` : downgraded ? `已请求快速模式，上游降级为${serviceMode(r.responseServiceTier)}` : '已请求快速模式';
  return <span className={`record-fast-mode ${state}`} role="img" aria-label={label} title={label}><Zap size={14} fill={confirmed ? 'currentColor' : 'none'} aria-hidden="true" /></span>;
}
const transportLabel = (value: string | null) => ({ ws: 'WS', sse: 'SSE', http: 'HTTP', native: '原生' }[value ?? ''] ?? '未知');
function TransportBadge({ value }: { value: string | null }) {
  return <span className={`transport-badge transport-${value ?? 'unknown'}`} title={value === 'native' ? '通过原生 app-server 执行，内部上游协议不可观测' : undefined}>{transportLabel(value)}</span>;
}
const responseMode = (r: RequestRecord) => r.kind !== 'inference' ? '—' : r.streaming === true ? '流式' : r.streaming === false ? '非流式' : '未知';
const schedulingModes: Record<string, string> = { hardBinding: '强绑定', softAffinity: '软粘性', balanced: '综合选择', exploration: '探索' };
const affinityReasons: Record<string, string> = { hardBinding: '沿用响应链账户', notRequested: '无可匹配的纯文本续聊', notFound: '未找到成功对话', expired: '已超过两分钟', routeChanged: '账户路由已变化', ambiguous: '匹配存在歧义', unavailable: '原账户不可用或不在最佳能力等级', scoreGap: '评分差距超过 0.08', hit: '命中成功对话' };

export function RequestRows({ records, onOpen }: { records: RequestRecord[]; onOpen: (record: RequestRecord, trigger: HTMLButtonElement) => void }) {
  return <tbody>{records.map(r => <tr key={r.id}>
    <td className="record-key"><strong title={r.keyName ?? r.keyId ?? undefined}>{r.keyName ?? (r.keyId ? `${r.keyId.slice(0, 8)}…` : '后台操作')}</strong><small>{requestKinds[r.kind] ?? r.kind}</small></td>
    <td className="record-account"><strong title={r.accountName ?? undefined}>{r.accountName ?? (r.kind === 'inference' ? '未分配账户' : '全局操作')}</strong><small title={r.groupName ?? undefined}>{r.groupName ?? '—'}</small></td>
    <td className="record-model"><strong title={r.kind === 'inference' ? '请求模型' : undefined}>{r.model ?? action(r)}<RequestFastMode record={r} /></strong>{r.kind === 'inference' && r.responseModel && r.responseModel !== r.model ? <small className="record-response-model" title={`实际响应模型：${r.responseModel}`}><CornerDownRight size={13} aria-hidden="true" /><span><span className="visually-hidden">实际响应模型：</span>{r.responseModel}</span></small> : null}</td>
    <td className="record-effort" data-label="推理强度" title={r.reasoningEffort === 'default' ? '请求未指定推理强度，使用原生配置或模型默认值' : undefined}>{r.kind === 'inference' ? effort(r) : '—'}{r.kind === 'inference' && r.subagentKind ? <span className="record-subagent" role="img" title={`子代理请求：${r.subagentKind}`} aria-label={`子代理请求：${r.subagentKind}`}><Bot size={14} aria-hidden="true" /></span> : null}</td>
    <td className="record-connection">
      <div className="record-endpoint" data-label="端点"><code>{r.endpoint ?? (r.kind === 'inference' ? '未知' : '—')}</code></div>
      <div className="record-transports"><span>上游 <span className="record-upstream">{r.kind === 'inference' ? <TransportBadge value={r.upstreamTransport} /> : '—'}</span></span><span>接入 <span className="record-client">{r.kind === 'inference' ? <TransportBadge value={r.clientTransport} /> : '—'}</span></span></div>
      <div className="record-agent" title={r.userAgent ?? undefined}>{r.userAgent ?? (r.kind === 'inference' ? '未知 UA' : '—')}</div>
    </td>
    <td className="record-tokens">
      {r.kind === 'inference' ? <><div className="token-flow"><span className="token-input" title="输入 Token"><ArrowDown size={12} aria-hidden="true" /><span className="visually-hidden">输入 </span><TokenCount value={r.inputTokens} unknown="—" /></span><span className="token-output" title="输出 Token"><ArrowUp size={12} aria-hidden="true" /><span className="visually-hidden">输出 </span><TokenCount value={r.outputTokens} unknown="—" /></span></div><div className="token-cache" title={`缓存输入 Token · 命中率 ${cacheRate(r)}`}><Database size={12} aria-hidden="true" /><span>缓存 <TokenCount value={r.cachedInputTokens} unknown="—" /></span></div><div className="token-cache-rate">命中 {cacheRate(r)}</div></> : <span className="record-muted">—</span>}
    </td>
    <td className="record-latency"><div title="从进入调度统计到记录首个有效输出（文本、推理摘要或工具调用），包含排队时间"><span>首响</span><strong>{formatDuration(r.firstResponseMs)}</strong></div><div title="从排队结束、开始执行到记录首个有效输出（文本、推理摘要或工具调用），不含排队时间"><span>模型首响</span><strong>{formatDuration(r.firstDeltaMs)}</strong></div><div title="从收到请求到服务端执行结算，包含排队与清理时间"><span>总耗时</span><strong>{r.totalMs == null && ['running', 'queued'].includes(r.status) ? requestStatuses[r.status] : formatDuration(r.totalMs)}</strong></div></td>
    <td className="record-result"><span className={`record-status ${r.status}`}>{requestStatuses[r.status] ?? r.status}</span>{r.errorCode ? <code title={r.errorCode}>{r.errorCode}</code> : null}</td>
    <td className="record-time"><time dateTime={new Date(r.startedAt).toISOString()} title={date(r.startedAt)}><strong>{new Date(r.startedAt).toLocaleTimeString('zh-CN', { hour12: false })}</strong><small>{new Date(r.startedAt).toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })}</small></time></td>
    <td className="record-open"><button type="button" className="icon-action" aria-label={`查看请求详情 ${r.id.slice(0, 8)}`} title="查看请求详情" onClick={event => onOpen(r, event.currentTarget)}><Eye size={16} aria-hidden="true" /></button></td>
  </tr>)}</tbody>;
}

export function RequestDetailsDialog({ token, record: r, close }: { token: string; record: RequestRecord; close: () => void }) {
  const id = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const element = dialog.current;
    const previous = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    element?.showModal(); closeButton.current?.focus();
    return () => { element?.close(); document.body.style.overflow = previous; };
  }, []);
  const dismiss = () => { dialog.current?.close(); close(); };
  const delivery: Record<string, string> = { handed_off: '已交给响应通道', disconnected: '客户端已断开', failed: '交付失败', terminal_pending: '等待交付记录' };
  const outcome: Record<string, string> = { completed: '已完成', failed: '失败', cancelled: '已取消' };
  const metrics = [
    ['首响', formatDuration(r.firstResponseMs)], ['模型首响', formatDuration(r.firstDeltaMs)], ['总耗时', formatDuration(r.totalMs)],
    ['排队耗时', formatDuration(r.waitMs)], ['执行耗时', formatDuration(r.durationMs)],
  ];
  const fields = [
    ['密钥名称', r.keyName], ['Key ID', r.keyId], ['账户', r.accountName], ['分组', r.groupName],
    ['请求模型', r.model], ['实际响应模型', r.responseModel ?? (r.kind === 'inference' ? '未记录' : null)], ['推理强度', effort(r)], ['请求服务模式', r.kind === 'inference' ? serviceMode(r.serviceTier) : '—'], ['实际服务模式', r.kind !== 'inference' ? '—' : r.responseServiceTier && r.responseServiceTier !== 'auto' ? serviceMode(r.responseServiceTier) : '未确认（上游未返回实际等级）'], ['User-Agent', r.userAgent], ['操作', action(r)], ['请求时间', date(r.startedAt)], ['请求 ID', r.id],
    ['端点', r.endpoint], ['上游', transportLabel(r.upstreamTransport)], ['接入', transportLabel(r.clientTransport)],
    ['响应类型', responseMode(r)],
    ['账户调度', r.scheduling ? schedulingModes[r.scheduling.mode] ?? r.scheduling.mode : '未知'],
    ['输入 Token', exactTokens(r.inputTokens)], ['输出 Token', exactTokens(r.outputTokens)],
    ['缓存 Token', exactTokens(r.cachedInputTokens)], ['缓存命中率', cacheRate(r)],
    ['原生线程', r.nativeThreadId], ['原生轮次', r.nativeTurnId], ['模型结果', r.modelOutcome ? outcome[r.modelOutcome] ?? r.modelOutcome : null],
    ['错误码', r.errorCode], ['失败阶段', r.failureStage], ['清理错误', r.cleanupErrorCode],
    ['交付记录', r.deliveryStatus ? delivery[r.deliveryStatus] ?? r.deliveryStatus : null],
  ];
  return <dialog ref={dialog} className="request-detail-dialog" aria-labelledby={`${id}-heading`} aria-describedby={`${id}-timing`} onCancel={event => { event.preventDefault(); dismiss(); }}>
    <div className="request-detail-heading"><div><h2 id={`${id}-heading`}>请求详情</h2><p>{r.model ?? action(r)} <span className={`record-status ${r.status}`}>{requestStatuses[r.status] ?? r.status}</span></p></div><button ref={closeButton} className="icon-action" type="button" aria-label="关闭请求详情" onClick={dismiss}><X size={18} aria-hidden="true" /></button></div>
    <div className="request-detail-body">
      <dl className="request-timing">{metrics.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
      <p id={`${id}-timing`} className="request-timing-note">首响从进入调度统计到首个有效输出（文本、推理摘要或工具调用），包含排队；模型首响从排队结束、开始执行到首个有效输出（文本、推理摘要或工具调用），不含排队；总耗时到服务端执行结算，包含排队、执行与清理。尚未记录的用量与耗时显示“—”或“未知”。</p>
      <dl className="request-fields">{fields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value ?? '—'}</dd></div>)}</dl>
      {<RequestFailureContent key={`${r.id}-${r.status}-${r.deliveryStatus ?? ''}`} token={token} requestId={r.id} />}
      {r.scheduling ? <details><summary>调度说明</summary><dl className="request-fields">
        <div><dt>算法版本</dt><dd>{r.scheduling.version}</dd></div>
        <div><dt>候选账户数</dt><dd>{r.scheduling.candidateCount}</dd></div>
        <div><dt>综合评分</dt><dd>{r.scheduling.score?.toFixed(3) ?? '—'}</dd></div>
        <div><dt>粘性判断</dt><dd>{affinityReasons[r.scheduling.affinity] ?? r.scheduling.affinity}</dd></div>
        {r.scheduling.components ? (['load', 'quota', 'health', 'latency'] as const).map(key => <div key={key}><dt>{{ load: '负载（45%）', quota: '额度（25%）', health: '稳定性（20%）', latency: '首字速度（10%）' }[key]}</dt><dd>{r.scheduling!.components![key].toFixed(3)}</dd></div>) : null}
      </dl><p className="request-timing-note">分项评分越高越优。软粘性无可用容量或评分差距过大时立即重新选择；强绑定遵循原响应链。</p></details> : null}
      {r.parameterHandling?.length ? <details><summary>参数兼容说明（{r.parameterHandling.length} 项）</summary><dl className="request-fields">{r.parameterHandling.map((note, index) => <div key={`${note.path}-${index}`}><dt>{note.path}</dt><dd>{({ native: '原生支持', adapted: '适配', ignored: '已忽略' } as Record<string, string>)[note.action] ?? note.action}：{note.reason}</dd></div>)}</dl></details> : null}
      <p className="request-timing-note">实际响应模型按上游响应或原生模型变更通知记录，未报告时显示“未记录”。交付记录表示服务端交给响应通道，不代表客户端已完整收到。调试模式保存请求和响应原文；未开启时，仅失败记录保存省略原文后的参数与错误响应。</p>
    </div>
  </dialog>;
}
