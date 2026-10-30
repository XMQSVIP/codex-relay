import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { Check, Copy, LoaderCircle, Monitor, Terminal, X } from 'lucide-react';
import type { ClientKey, ClientKeys, Operations } from './api';
import { clientEndpoints } from './clientEndpoints';
import { buildCodexConfig, keyModelChoices, keyUsageUnavailable, validCodexModel } from './codexConfig';
import { copyText } from './clipboard';
import { useToast } from './Toast';
import './key-usage.css';

export function KeyUsage({ value, data, operations, allowPublicAccess, close }: {
  value: ClientKey | undefined; data: ClientKeys; operations: Operations;
  allowPublicAccess: boolean; close: () => void;
}) {
  const id = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const addressField = useRef<HTMLSelectElement>(null);
  const preview = useRef<HTMLTextAreaElement>(null);
  const copyAttempt = useRef(0);
  const notify = useToast();
  const [platform, setPlatform] = useState(() => /Windows/i.test(navigator.userAgent) ? 'windows' : 'unix');
  const [selectedAddress, setSelectedAddress] = useState('');
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [websocketEnabled, setWebsocketEnabled] = useState(false);
  const [copyState, setCopyState] = useState<'idle' | 'copying' | 'copied' | 'error'>('idle');
  const { choices: addresses, preferred: preferredAddress } = clientEndpoints(data, allowPublicAccess, location.origin);
  const baseUrl = addresses.has(selectedAddress) ? selectedAddress : preferredAddress;
  const { choices: models, preferred: preferredModel } = value ? keyModelChoices(value, operations) : { choices: [], preferred: '' };
  const restricted = Boolean(value?.models.length);
  const model = restricted
    ? selectedModel !== null && models.includes(selectedModel) ? selectedModel : preferredModel
    : selectedModel ?? preferredModel;
  const validModel = validCodexModel(model.trim()) && (!restricted || models.includes(model.trim()));
  const unavailable = keyUsageUnavailable(value);
  const config = !unavailable && validModel && baseUrl && value?.secret
    ? buildCodexConfig({ secret: value.secret, baseUrl, model: model.trim(), websocketEnabled }) : '';
  const configPath = platform === 'windows' ? '%USERPROFILE%\\.codex\\config.toml' : '~/.codex/config.toml';
  const copying = copyState === 'copying';
  const dismiss = useCallback(() => { dialog.current?.close(); close(); }, [close]);

  useEffect(() => {
    const previous = document.body.style.overflow;
    const element = dialog.current;
    document.body.style.overflow = 'hidden';
    element?.showModal();
    addressField.current?.focus();
    return () => { copyAttempt.current++; element?.close(); document.body.style.overflow = previous; };
  }, []);

  useEffect(() => {
    if (unavailable) { dismiss(); notify(unavailable, 'error'); return; }
    if (value?.expiresAt == null) return;
    // Recheck long lifetimes without overflowing the browser's signed 32-bit timeout.
    let timer: ReturnType<typeof setTimeout>;
    const checkExpiry = () => {
      const remaining = value.expiresAt! - Date.now();
      if (remaining <= 0) { dismiss(); notify('密钥已过期，请创建新密钥。', 'error'); }
      else timer = setTimeout(checkExpiry, Math.min(remaining, 2_147_483_647));
    };
    checkExpiry();
    return () => clearTimeout(timer);
  }, [unavailable, value?.expiresAt, dismiss, notify]);

  useEffect(() => { copyAttempt.current++; setCopyState('idle'); }, [config]);

  const copy = async () => {
    const reason = keyUsageUnavailable(value);
    if (reason) { dismiss(); notify(reason, 'error'); return; }
    if (!config || copying) return;
    const attempt = ++copyAttempt.current;
    setCopyState('copying');
    try {
      await copyText(config);
      if (copyAttempt.current !== attempt) return;
      setCopyState('copied'); notify('Codex 配置已复制。');
    } catch {
      if (copyAttempt.current !== attempt) return;
      setCopyState('error'); notify('复制失败，请在配置框中手动选择并复制。', 'error');
      preview.current?.focus(); preview.current?.select();
    }
  };

  return <dialog ref={dialog} className="key-usage-dialog" aria-labelledby={`${id}-heading`} aria-describedby={`${id}-description`}
    onCancel={event => { event.preventDefault(); dismiss(); }} onKeyDown={event => {
      if (event.key !== 'Tab') return;
      const controls = [...event.currentTarget.querySelectorAll<HTMLElement>('button, input, select, textarea, a[href], [tabindex]')]
        .filter(element => element.tabIndex >= 0 && !element.matches(':disabled') && element.getClientRects().length > 0);
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }}>
    <div className="section-heading"><div><h2 id={`${id}-heading`}>使用密钥</h2><p id={`${id}-description`}>{value?.name} · 配置 Codex CLI / 桌面端</p></div>
      <button type="button" className="button secondary key-dialog-close" aria-label="关闭使用密钥窗口" onClick={dismiss}><X size={18} aria-hidden="true" /></button>
    </div>
    <div className="key-usage-fields">
      <div><label htmlFor={`${id}-address`}>接入地址</label><select ref={addressField} id={`${id}-address`} value={baseUrl} onChange={event => setSelectedAddress(event.target.value)} disabled={copying} aria-describedby={`${id}-address-help`}>
        {[...addresses].map(([url, label]) => <option key={url} value={url}>{label} · {url}</option>)}
      </select><p id={`${id}-address-help`} className="helper">{allowPublicAccess ? '其他设备请选择可达的 IP 或域名；监听地址不代表网络已连通。' : '其他设备接入前，请在“系统设置 → 访问安全”中开启外部访问。'}</p></div>
      <div><label htmlFor={`${id}-model`}>模型 ID</label>{restricted
        ? <select id={`${id}-model`} value={model} onChange={event => setSelectedModel(event.target.value)} disabled={copying} aria-describedby={`${id}-model-help`}>{models.map(candidate => <option key={candidate} value={candidate}>{candidate}</option>)}</select>
        : <><input id={`${id}-model`} list={`${id}-models`} value={model} onChange={event => setSelectedModel(event.target.value)} disabled={copying} maxLength={128} placeholder="输入客户端要使用的模型 ID" aria-invalid={!validModel} aria-describedby={`${id}-model-help`} /><datalist id={`${id}-models`}>{models.map(candidate => <option key={candidate} value={candidate} />)}</datalist></>}
        <p id={`${id}-model-help`} className="helper">{!validModel ? '请输入有效模型 ID（字母、数字或 . _ : / -，最多 128 个字符）。' : restricted ? '仅可选择此密钥允许的模型，实际支持情况由上游决定。' : '默认 gpt-6.1-sol；也可选择同分组的路由模型或输入其他模型 ID。'}</p>
      </div>
    </div>
    <div className="key-usage-options"><div role="group" aria-label="客户端操作系统" className="example-switch">
      <button type="button" aria-pressed={platform === 'windows'} onClick={() => setPlatform('windows')}><Monitor size={16} aria-hidden="true" />Windows</button>
      <button type="button" aria-pressed={platform === 'unix'} onClick={() => setPlatform('unix')}><Terminal size={16} aria-hidden="true" />macOS / Linux</button>
    </div><label className="checkbox-label"><input type="checkbox" checked={websocketEnabled} onChange={event => setWebsocketEnabled(event.target.checked)} disabled={copying} />启用 WebSocket</label></div>
    <section className="key-usage-preview" aria-label="Codex 配置">
      <div className="key-usage-preview-heading"><code>{configPath}</code><span>{websocketEnabled ? 'WebSocket' : 'HTTP / SSE'}</span></div>
      <label className="visually-hidden" htmlFor={`${id}-config`}>Codex 配置内容</label>
      <textarea ref={preview} id={`${id}-config`} readOnly spellCheck={false} value={config} rows={16} aria-describedby={`${id}-config-help`} placeholder="填写有效模型 ID 后生成配置" />
    </section>
    <p id={`${id}-config-help`} className="helper">配置框可滚动查看，复制时包含全部内容。</p>
    {copyState === 'error' ? <p className="field-error" role="status">自动复制不可用，已选中配置内容，请手动复制。</p> : null}
    <div className="key-usage-instructions"><p>将以上内容合并到客户端的用户级 <code>config.toml</code>，保留原有其他配置，然后完全退出并重启 Codex。已设置 <code>CODEX_HOME</code> 时，使用该目录下的 <code>config.toml</code>。</p>
      <p>配置包含完整密钥，无需修改 <code>auth.json</code>。客户端与中转站同机部署时，请让服务端使用指向真实上游的独立账户配置，避免请求回到中转站自身。</p>
    </div>
    <div className="key-usage-actions"><button type="button" className="button secondary" onClick={dismiss}>关闭</button><button type="button" className="button primary" disabled={!config || copying} onClick={() => void copy()}>
      {copying ? <LoaderCircle className="spin" size={16} aria-hidden="true" /> : copyState === 'copied' ? <Check size={16} aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}{copying ? '正在复制' : copyState === 'copied' ? '已复制配置' : '复制配置'}
    </button></div>
  </dialog>;
}
