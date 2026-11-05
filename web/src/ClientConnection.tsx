import { useId, useState } from 'react';
import { Check, ChevronDown, Copy, Globe2, LoaderCircle, Monitor, Radio } from 'lucide-react';
import type { ClientKeys } from './api';
import { copyText } from './clipboard';
import { useToast } from './Toast';
import { clientEndpoints } from './clientEndpoints';
import './client-examples.css';

const firstInput = [{ role: 'user', content: '什么是缓存？' }];
const exampleOutput = [{
  type: 'message', id: 'msg_REPLACE_WITH_ACTUAL_ID', role: 'assistant', status: 'completed',
  content: [{ type: 'output_text', text: '缓存会保存可复用的数据，以减少重复处理。', annotations: [] }],
}];
const multiTurnRequest = (continuing: boolean) => JSON.stringify({
  model: 'YOUR_MODEL_ID',
  client_metadata: { thread_id: 'conversation-REPLACE_WITH_UUID' },
  instructions: '请用中文回答。',
  input: continuing
    ? [...firstInput, ...exampleOutput, { role: 'user', content: '请给一个例子。' }]
    : firstInput,
  stream: false,
  store: false,
}, null, 2);

export function ClientConnection({ data, allowPublicAccess }: { data: ClientKeys | null; allowPublicAccess: boolean }) {
  const id = useId();
  const notify = useToast();
  const [selected, setSelected] = useState('');
  const [copied, setCopied] = useState('');
  const [copying, setCopying] = useState('');
  const [manualCopy, setManualCopy] = useState('');
  const [example, setExample] = useState('curl');
  const [continuing, setContinuing] = useState(false);
  const [exampleOpen, setExampleOpen] = useState(() => window.matchMedia('(min-width: 1201px)').matches);
  const { choices, preferred } = clientEndpoints(data, allowPublicAccess, location.origin);
  const baseUrl = choices.has(selected) ? selected : preferred;
  const nativeUrl = baseUrl ? `${baseUrl.replace(/^http/, 'ws').replace(/\/$/, '')}/native` : '';
  const exampleBaseUrl = (baseUrl || 'http://127.0.0.1:4317/v1').replace(/\/$/, '');
  const exampleCode = example === 'multiturn' ? multiTurnRequest(continuing) : example === 'curl'
    ? `curl ${exampleBaseUrl}/responses \\\n  -H "Authorization: Bearer YOUR_API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model":"YOUR_MODEL_ID","input":"你好","stream":false}'`
    : `import os\nfrom openai import OpenAI\n\nclient = OpenAI(\n    base_url=${JSON.stringify(exampleBaseUrl)},\n    api_key=os.environ["CODEX_RELAY_API_KEY"],\n)\nresponse = client.responses.create(\n    model="YOUR_MODEL_ID",\n    input="你好",\n    stream=False,\n)\nprint(response.output_text)`;
  const copy = async (value: string) => {
    if (copying) return;
    setCopying(value); setCopied(''); setManualCopy('');
    try { await copyText(value); setCopied(value); notify(value === exampleCode ? '接入示例已复制。' : '接入地址已复制。'); }
    catch { setCopied(''); setManualCopy(value); notify('浏览器未允许复制，请选择下方内容手动复制。', 'error'); }
    finally { setCopying(''); }
  };
  return <div className="client-connect-layout"><section className="panel client-connection" id="key-connection-settings" aria-labelledby={`${id}-heading`}>
    <div className="client-connection-heading"><div><h2 id={`${id}-heading`}>连接你的客户端</h2><p>复制地址，使用“API 密钥”中创建的密钥发起请求。</p></div>
      <div className="client-connection-status"><span className="client-access-scope">{allowPublicAccess ? <Globe2 size={15} aria-hidden="true" /> : <Monitor size={15} aria-hidden="true" />}{allowPublicAccess ? '允许外部访问' : '仅本机访问'}</span><span className={`badge ${data?.inferenceEnabled ? 'success' : 'neutral'}`}>{!data ? '读取状态…' : data.inferenceEnabled ? '接入已启用' : '待启用账户'}</span></div>
    </div>
    <div className="client-address-field"><label htmlFor={`${id}-url`}>HTTP Base URL</label><div className="client-address-controls">
      <select id={`${id}-url`} value={baseUrl} onChange={event => { setSelected(event.target.value); setCopied(''); setManualCopy(''); }} disabled={!data} aria-describedby={`${id}-help`}>
        {!data ? <option value="">正在读取接入地址…</option> : [...choices].map(([url, label]) => <option key={url} value={url}>{label} · {url}</option>)}
      </select>
      <button type="button" className="button secondary" disabled={!data || !baseUrl || !!copying} onClick={() => void copy(baseUrl)}>{copying === baseUrl && baseUrl ? <LoaderCircle className="spin" size={16} aria-hidden="true" /> : copied === baseUrl && baseUrl ? <Check size={16} aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}{copying === baseUrl && baseUrl ? '正在复制' : copied === baseUrl && baseUrl ? '已复制' : '复制地址'}</button>
    </div><p id={`${id}-help`} className="helper">{allowPublicAccess ? '其他设备可使用可达的 IP 或域名；监听地址不代表网络已连通。' : '其他设备接入前，需在“系统设置 → 访问安全”中开启外部访问。'}</p></div>
    {manualCopy ? <div className="client-copy-fallback"><p>请选择下方内容手动复制。</p><label className="visually-hidden" htmlFor={`${id}-manual`}>手动复制内容</label><textarea id={`${id}-manual`} readOnly autoFocus rows={manualCopy.includes('\n') ? 6 : 2} value={manualCopy} onFocus={event => event.currentTarget.select()} /></div> : null}
    <details className="client-connection-advanced"><summary>WebSocket 与监听详情</summary><div className="client-advanced-content">
      <div className="client-native-address"><label htmlFor={`${id}-native`}><Radio size={15} aria-hidden="true" />原生 WebSocket</label><div className="client-address-controls"><input id={`${id}-native`} value={nativeUrl} readOnly onFocus={event => event.currentTarget.select()} /><button type="button" className="button secondary" disabled={!nativeUrl || !!copying} onClick={() => void copy(nativeUrl)}>{copied === nativeUrl && nativeUrl ? <Check size={16} aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}{copied === nativeUrl && nativeUrl ? '已复制' : '复制 WebSocket'}</button></div><p className="helper">仅原生会话客户端需要；普通 HTTP / SSE 客户端使用上方 Base URL。</p></div>
      {data?.networkEndpoints.length ? <div><h3>已配置的监听入口</h3><ul className="client-listeners">{data.networkEndpoints.map((endpoint, index) => <li key={`${endpoint.baseUrl}-${index}`}><code>{endpoint.baseUrl}</code><span>{endpoint.transport === 'reverse-proxy' ? 'HTTPS 反向代理' : endpoint.transport.toUpperCase()} · 监听 {endpoint.listen}</span></li>)}</ul></div> : <p className="helper">尚未配置独立监听入口，当前使用后台所在端口。</p>}
      {baseUrl.startsWith('http:') && baseUrl !== data?.localBaseUrl ? <p className="helper">当前 HTTP 地址使用明文传输；需要加密时请选择 HTTPS 入口。</p> : null}
    </div></details>
  </section><details className="panel client-example" open={exampleOpen} onToggle={event => setExampleOpen(event.currentTarget.open)}>
    <summary className="client-example-summary"><h2>API 接入示例</h2><span>请求与续聊</span><ChevronDown size={16} aria-hidden="true" /></summary>
    <div className="client-example-heading">
      <div role="group" aria-label="接入示例语言" className="example-switch">
        {[['curl', 'cURL'], ['python', 'Python'], ['multiturn', '多轮 JSON']].map(([value, label]) => <button type="button" key={value} aria-pressed={example === value} onClick={() => { setExample(value); setCopied(''); setManualCopy(''); }}>{label}</button>)}
      </div>
      <button type="button" className="copy-button" aria-label={copying === exampleCode ? '正在复制接入示例' : example === 'multiturn' ? '复制请求 JSON' : '复制接入示例'} title={example === 'multiturn' ? '复制请求 JSON' : '复制接入示例'} disabled={!!copying} onClick={() => void copy(exampleCode)}>{copying === exampleCode ? <LoaderCircle className="spin" size={16} aria-hidden="true" /> : copied === exampleCode ? <Check size={16} aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}</button>
    </div>
    {example === 'multiturn' ? <div className="client-multiturn-intro">
      <p>普通 API 客户端无需安装 Codex。每轮向下面的地址发送 JSON：</p>
      <code className="client-multiturn-endpoint">POST {exampleBaseUrl}/responses</code>
      <p>请求头：<code>Authorization: Bearer YOUR_API_KEY</code><br /><code>Content-Type: application/json</code></p>
      <div role="group" aria-label="多轮请求步骤" className="example-switch">
        {[{ value: false, label: '第一轮' }, { value: true, label: '第二轮（续聊）' }].map(({ value, label }) => <button type="button" key={String(value)} aria-pressed={continuing === value} onClick={() => { setContinuing(value); setCopied(''); setManualCopy(''); }}>{label}</button>)}
      </div>
      <p>{continuing ? '下一轮 input = 上一轮 input + 上一轮完整 output + 新的用户消息。' : '替换模型 ID，并为这段对话生成一个 UUID；之后每轮保持相同的 thread_id。'}</p>
      {continuing ? <p className="client-multiturn-replace">下方 assistant 条目仅展示结构。发送前请替换为上一轮实际返回的完整 output，不要照抄示例答案或消息 ID。</p> : null}
    </div> : null}
    <pre tabIndex={0} aria-label={`${example === 'multiturn' ? '多轮 JSON' : example === 'curl' ? 'cURL' : 'Python'} 接入示例`}><code>{exampleCode}</code></pre>
    {example === 'multiturn' ? <div className="client-multiturn-rules">
      <h3>保持对话连续</h3>
      <ul>
        <li>同一对话固定 <code>client_metadata.thread_id</code>；新对话、分支使用新 ID，不要每次请求重新生成。</li>
        <li>原样保留完整 <code>output</code>，包括 reasoning、加密内容、工具调用及消息字段；只复制回答文字可能无法续接。</li>
        <li>历史只追加；尽量保持模型、instructions 和工具目录稳定。同一对话等上一轮完成后再发送下一轮。</li>
        <li>原生账户的 HTTP 续聊需要完整历史，不能只传 <code>previous_response_id</code>；<code>prompt_cache_key</code> 不是必填项。</li>
      </ul>
      <p>使用工具时，先保留完整 output，再追加对应 call_id 的工具结果，并保持 tools 定义一致。</p>
      <p>固定会话有助于复用，但不保证缓存命中。查看 <code>usage.input_tokens_details.cached_tokens</code>；未报告表示未知。示例只展示请求格式，不会自动执行。</p>
    </div> : <p className="client-example-note">{example === 'curl' ? '适用于 Bash / zsh；' : '安装 openai 包并设置环境变量 CODEX_RELAY_API_KEY；'}替换模型 ID{example === 'curl' ? ' 和 API Key' : ''}后运行。示例不会自动执行。</p>}
  </details></div>;
}
