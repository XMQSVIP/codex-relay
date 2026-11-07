import { useEffect, useState } from 'react';
import { ChevronDown, Copy, LoaderCircle } from 'lucide-react';
import { api } from './api';
import type { RequestContent } from './api';
import { copyText } from './clipboard';
import { useToast } from './Toast';
import './request-content.css';

const transports: Record<string, string> = { http: 'HTTP', sse: 'SSE', websocket: 'WebSocket', internal: '执行错误' };

export function RequestFailureContent({ token, requestId }: { token: string; requestId: string }) {
  const [content, setContent] = useState<RequestContent | null>(null);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const [copying, setCopying] = useState('');
  const notify = useToast();
  useEffect(() => {
    const controller = new AbortController();
    setContent(null); setError('');
    void api<RequestContent>(token, `/requests/${encodeURIComponent(requestId)}/content`, undefined, controller.signal)
      .then(value => { if (!controller.signal.aborted) setContent(value); })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : '请求内容加载失败。'); });
    return () => controller.abort();
  }, [token, requestId, attempt]);
  useEffect(() => {
    if (!content?.request?.debug && !content?.response?.debug) return;
    const controller = new AbortController();
    const timer = window.setInterval(() => {
      void api<{ enabled: boolean; revision: number }>(token, '/settings/debug', undefined, controller.signal)
        .then(value => { if (!controller.signal.aborted && (!value.enabled || (content?.request?.debugRevision != null && content.request.debugRevision !== value.revision))) setContent({ id: requestId, request: null, response: null }); })
        .catch(() => {});
    }, 2000);
    return () => { window.clearInterval(timer); controller.abort(); };
  }, [token, requestId, content?.request?.debug, content?.response?.debug, content?.request?.debugRevision]);
  const requestJson = content?.request ? (content.request.debug ? String(content.request.body) : JSON.stringify(content.request.body, null, 2)) : '';
  const responseJson = content?.response ? (content.response.debug ? String(content.response.body) : JSON.stringify(content.response.body, null, 2)) : '';
  const copy = async (value: string, label: string, trigger: HTMLButtonElement) => {
    if (copying) return;
    setCopying(label);
    try {
      await copyText(value);
      if (trigger.isConnected) notify(`${label}已复制。`);
    } catch {
      if (trigger.isConnected) notify(`${label}复制失败，请手动选择对应 JSON。`, 'error');
    } finally { setCopying(''); }
  };

  return <section className="request-failure-content" aria-label="请求与响应内容">
    <h3>请求与响应内容</h3>
    {!content && !error ? <p role="status">正在加载请求与响应内容…</p> : null}
    {error ? <div role="alert"><p className="field-error">{error}</p><button type="button" className="text-button" onClick={() => setAttempt(value => value + 1)}>重试加载内容</button></div> : null}
    {content ? <>
      <details open><summary><span>原请求内容</span><ChevronDown size={16} aria-hidden="true" /></summary>
        {content.request ? <>
          <div className="request-content-toolbar"><p className="request-timing-note">{content.request.debug ? `调试原文 · ${content.request.encoding === 'base64' ? 'Base64 二进制内容' : 'UTF-8'}${content.request.complete === false ? ' · 未接收完整' : ''}` : '保留请求参数与消息结构，消息原文、指令及附件内容已省略。'}</p><button type="button" className="text-button" aria-label="复制原请求内容" disabled={!!copying} onClick={event => void copy(requestJson, '原请求内容', event.currentTarget)}>{copying === '原请求内容' ? <LoaderCircle className="spin" size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}{copying === '原请求内容' ? '正在复制' : '复制'}</button></div>
          {content.request.truncated ? <p className="request-timing-note">请求内容较大，部分参数或条目已截断。</p> : null}
          <pre tabIndex={0} aria-label="原请求 JSON">{requestJson}</pre>
        </> : <p className="request-timing-note">此记录未保存原请求内容。</p>}
      </details>
      <details open><summary><span>响应内容</span><ChevronDown size={16} aria-hidden="true" /></summary>
        {content.response ? <>
          <div className="request-content-toolbar"><p className="request-timing-note">{transports[content.response.transport] ?? content.response.transport}{content.response.statusCode != null ? `${content.response.transport === 'http' ? ' ' : ' · HTTP '}${content.response.statusCode}` : ''}{content.response.debug ? ` · 调试原文${content.response.encoding === 'base64' ? ' · Base64' : ''}${content.response.complete === false ? ' · 未接收完整' : ''}` : ['sse', 'websocket'].includes(content.response.transport) ? ' · 终态事件' : ''}</p><button type="button" className="text-button" aria-label="复制响应内容" disabled={!!copying} onClick={event => void copy(responseJson, '响应内容', event.currentTarget)}>{copying === '响应内容' ? <LoaderCircle className="spin" size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}{copying === '响应内容' ? '正在复制' : '复制'}</button></div>
          {content.response.truncated ? <p className="request-timing-note">响应内容较大，部分内容已截断。</p> : null}
          <pre tabIndex={0} aria-label="响应 JSON">{responseJson}</pre>
        </> : <p className="request-timing-note">此记录未保存响应内容。</p>}
      </details>
    </> : null}
  </section>;
}
