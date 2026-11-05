import { useEffect, useId, useRef, useState } from 'react';
import { MessageSquare, Send, Square, Trash2 } from 'lucide-react';
import type { Account, AccountPolicy } from './api';
import { Modal } from './ui';
import { formatDuration } from './RequestRecords';
import { readTestChat } from './testChatStream';
import { testChatPresets } from './testChatPresets';
import type { ChatEvent } from './testChatStream';
import './account-test-chat.css';

type Props = { account: Account; policy?: AccountPolicy; proxy: string; token: string; disabled?: boolean; iconOnly?: boolean };
type Turn = { prompt: string; reply: string; state: 'pending' | 'done' | 'error' | 'stopped'; metrics?: ChatEvent; error?: string };
const bytes = (text: string) => new TextEncoder().encode(text).length;

export function AccountTestChatButton(props: Props) {
  const [open, setOpen] = useState(false);
  return <><button type="button" className={props.iconOnly ? 'account-icon-button' : 'button secondary'} title="测试对话" aria-label={`测试对话：${props.account.name}`} disabled={props.disabled} onClick={() => setOpen(true)}><MessageSquare size={18} aria-hidden="true" />{props.iconOnly ? null : '测试对话'}</button>{open ? <TestChat {...props} close={() => setOpen(false)} /> : null}</>;
}

function TestChat({ account, policy, proxy, token, close }: Props & { close: () => void }) {
  const id = useId();
  const [model, setModel] = useState(policy?.route?.model ?? '');
  const [reasoningEffort, setReasoningEffort] = useState('low');
  const [actualModel, setActualModel] = useState('');
  const [draft, setDraft] = useState('ciallo');
  const [customModel, setCustomModel] = useState(false);
  const models = [...new Set([policy?.route?.model, ...(policy?.capabilities?.models.map(item => item.model || item.id) ?? [])].filter((item): item is string => !!item))];
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('准备就绪');
  const [error, setError] = useState('');
  const controller = useRef<AbortController | null>(null);
  const mounted = useRef(true), end = useRef<HTMLDivElement>(null), follow = useRef(true);
  const input = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { mounted.current = true; input.current?.focus(); return () => { mounted.current = false; controller.current?.abort(); }; }, []);
  useEffect(() => { if (follow.current) end.current?.scrollIntoView({ block: 'nearest' }); }, [turns]);
  const clear = () => { setTurns([]); setError(''); setStatus('准备就绪'); setActualModel(''); setDraft('ciallo'); input.current?.focus(); };
  const send = async (message = draft) => {
    if (controller.current || !message.trim()) return;
    const successful = turns.filter(turn => turn.state === 'done');
    const messages = successful.flatMap(turn => [{ role: 'user', content: turn.prompt }, { role: 'assistant', content: turn.reply }]);
    messages.push({ role: 'user', content: message.trim() });
    if (messages.length > 31 || messages.some(message => bytes(message.content) > 32768) || messages.reduce((size, message) => size + bytes(message.content), 0) > 48 * 1024) {
      setError('对话已超过测试长度限制（最多 16 轮、单条 32 KiB、合计 48 KiB），请缩短输入或清空对话。'); return;
    }
    if (account.source === 'api-key' && !model) { setError('请先输入要测试的上游模型 ID。'); return; }
    if (model && !/^[A-Za-z0-9._:/-]{1,128}$/.test(model)) { setError('请输入有效的模型 ID。'); return; }
    const abort = new AbortController(); controller.current = abort;
    const prompt = message.trim(); let reply = '', received = 0;
    const index = turns.length;
    const update = (patch: Partial<Turn>) => { if (mounted.current) setTurns(old => old.map((turn, i) => i === index ? { ...turn, ...patch } : turn)); };
    setTurns(old => [...old, { prompt, reply: '', state: 'pending' }]);
    setDraft(''); setError(''); setBusy(true); setStatus('正在连接账户…'); follow.current = true;
    try {
      const response = await fetch(`/api/accounts/${account.id}/chat-test`, {
        method: 'POST', headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
        body: JSON.stringify({ model: model || null, reasoningEffort: reasoningEffort || null, messages }), signal: abort.signal,
      });
      if (!response.ok) {
        const data = await response.json().catch(() => null);
        if (data?.error?.code === 'ADMIN_LOGIN_REQUIRED') window.dispatchEvent(new Event('bridge-auth-required'));
        throw new Error(data?.error?.message ?? `请求未完成（${response.status}）`);
      }
      if (!response.body || !response.headers.get('content-type')?.startsWith('text/event-stream')) throw new Error('服务未返回测试数据流。');
      await readTestChat(response.body, event => {
        if (!mounted.current || abort.signal.aborted) return;
        if (event.type === 'ready') { setActualModel(event.model ?? model); setStatus('等待回复…'); }
        if (event.type === 'delta') {
          if (typeof event.text !== 'string') throw new Error('测试回复格式无效。');
          received += bytes(event.text);
          if (received > 1024 * 1024) throw new Error('测试回复超过 1 MiB，已停止。');
          reply += event.text; update({ reply }); setStatus('正在回复…');
        }
        if (event.type === 'done') {
          if (!reply.trim()) throw new Error('请求结束，但没有收到文本回复。');
          update({ state: 'done', metrics: event }); setStatus('测试成功');
          if (event.model) setActualModel(event.model);
        }
        if (event.type === 'error') throw new Error(`${event.message ?? '测试失败'}${event.code ? `（${event.code}）` : ''}`);
      });
    } catch (cause) {
      if (mounted.current) {
        const stopped = abort.signal.aborted;
        const message = stopped ? '已停止，当前回复未完成。' : cause instanceof TypeError ? '无法连接服务，请检查连接后重试。' : cause instanceof Error ? cause.message : '测试失败，请重试。';
        update({ state: stopped ? 'stopped' : 'error', error: message }); setStatus(stopped ? '已停止' : '测试失败'); setDraft(prompt);
      }
    } finally { controller.current = null; if (mounted.current) { setBusy(false); input.current?.focus(); } }
  };
  return <Modal title="测试对话" trapFocus close={() => { controller.current?.abort(); close(); }}>
    <div className="account-test-chat">
      <div className="test-chat-context"><strong>{account.name}</strong><span title={proxy}>代理：{proxy}</span><small>仅测试当前账户 · 对话仅在窗口内保留，关闭即清空</small></div>
      <div className="test-chat-controls">
        <div className="test-chat-model"><label htmlFor={`${id}-model`}>测试模型</label>{models.length ? <select id={`${id}-model`} value={customModel ? '__custom__' : model} disabled={busy || turns.length > 0} onChange={event => { const custom = event.target.value === '__custom__'; setCustomModel(custom); setModel(custom ? '' : event.target.value); }}>{account.source !== 'api-key' ? <option value="">使用账户默认模型</option> : null}{models.map(value => <option key={value} value={value}>{value}</option>)}<option value="__custom__">手动输入模型 ID</option></select> : <input id={`${id}-model`} value={model} placeholder={account.source === 'api-key' ? '输入上游模型 ID' : '使用账户默认模型'} maxLength={128} disabled={busy || turns.length > 0} onChange={event => setModel(event.target.value)} />}</div>
        <div className="test-chat-reasoning"><label htmlFor={`${id}-reasoning`}>推理强度</label><select id={`${id}-reasoning`} value={reasoningEffort} disabled={busy} onChange={event => setReasoningEffort(event.target.value)}>{[['', '默认'], ['none', '不推理（none）'], ['minimal', '最低（minimal）'], ['low', '低（low）'], ['medium', '中（medium）'], ['high', '高（high）'], ['xhigh', '更高（xhigh）'], ['max', '最大（max）'], ['ultra', '极致（ultra）']].map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></div>
        <button type="button" className="text-button" onClick={clear} disabled={busy || !turns.length}><Trash2 size={15} aria-hidden="true" />清空对话</button>
      </div>
      {customModel && models.length ? <label className="test-chat-custom-model" htmlFor={`${id}-custom-model`}>自定义模型 ID<input id={`${id}-custom-model`} value={model} placeholder="输入上游模型 ID" maxLength={128} disabled={busy || turns.length > 0} onChange={event => setModel(event.target.value)} /></label> : null}
      <div className="test-chat-presets"><span>快捷提问</span>{testChatPresets.map(preset => <button key={preset.label} type="button" className="button secondary" disabled={busy} onClick={() => void send(preset.prompt)}>{preset.label}</button>)}<small>点击即发送</small></div>
      <div className="test-chat-transcript" role="log" aria-label="临时测试对话" aria-live="off" onScroll={event => { const node = event.currentTarget; follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 64; }}>
        {!turns.length ? <div className="test-chat-empty"><MessageSquare size={28} aria-hidden="true" /><p>发送一条消息，查看账户是否能正常回复。</p><small>支持临时多轮对话，可随时停止。</small></div> : turns.map((turn, index) => <div className="test-chat-turn" key={index}><div className="test-chat-message user"><small>你</small><p>{turn.prompt}</p></div><div className="test-chat-message assistant"><small>助手</small><p>{turn.reply || (turn.state === 'pending' ? '等待回复…' : '未收到回复')}</p>{turn.error ? <p className="test-chat-error" role="alert">{turn.error}</p> : null}{turn.metrics ? <div className="test-chat-metrics"><span>首响 {formatDuration(turn.metrics.firstResponseMs)}</span><span>总耗时 {formatDuration(turn.metrics.totalMs)}</span><span>Token {turn.metrics.usage?.totalTokens?.toLocaleString() ?? '—'}</span></div> : null}</div></div>)}<div ref={end} />
      </div>
      <div className="test-chat-status" role="status"><span>{status}</span>{actualModel ? <span title={actualModel}>{actualModel}</span> : null}</div>
      {error ? <p className="test-chat-error" role="alert">{error}</p> : null}
      <form onSubmit={event => { event.preventDefault(); void send(); }}><label className="visually-hidden" htmlFor={`${id}-prompt`}>测试消息</label><textarea ref={input} id={`${id}-prompt`} rows={3} value={draft} disabled={busy} placeholder="输入测试消息…" onChange={event => setDraft(event.target.value)} onKeyDown={event => { if ((event.ctrlKey || event.metaKey) && event.key === 'Enter' && !event.nativeEvent.isComposing) { event.preventDefault(); void send(); } }} /><div className="test-chat-compose-actions"><small>Ctrl / ⌘ + Enter 发送</small>{busy ? <button type="button" className="button secondary" onClick={event => { event.preventDefault(); controller.current?.abort(); setStatus('正在停止…'); }}><Square size={15} aria-hidden="true" />停止生成</button> : <button type="submit" className="button primary" disabled={!draft.trim()}><Send size={16} aria-hidden="true" />发送</button>}</div></form>
    </div>
  </Modal>;
}
