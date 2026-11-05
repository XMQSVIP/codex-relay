import { useId, useState } from 'react';
import type { ReactNode } from 'react';
import { KeyRound, LogIn, Monitor, Upload, Unplug } from 'lucide-react';
import { api } from './api';
import type { Account, AccountPolicy } from './api';
import { useToast } from './Toast';
import { AccountCredentialExport } from './AccountCredentialExport';

type Group = 'login' | 'credentials';
type Feedback = { group: Group; message: string; error?: boolean };
const credentialNames = {
  'api-key': 'API Key',
  managed: '原生管理凭据 · 自动续期',
  external: 'Access Token · 手动续期',
  desktop: '已绑定本机 Codex 登录',
  'native-login': '独立原生登录',
};

function AccountGroup({ id, title, description, icon, children }: {
  id: string; title: string; description: string; icon: ReactNode; children: ReactNode;
}) {
  return <section className="account-group" aria-labelledby={`${id}-heading`}>
    <div className="account-group-title-row"><span aria-hidden="true">{icon}</span><div><h3 id={`${id}-heading`}>{title}</h3><p className="helper">{description}</p></div></div>
    <div className="account-group-body">{children}</div>
  </section>;
}

export function OAuthAccount({ id, token, policy, credentialSource, disabled, loginActions, loginResult, onChange }: {
  id: string; token: string; policy?: AccountPolicy; credentialSource?: Account['credentialSource'];
  disabled: boolean; loginActions: ReactNode; loginResult: ReactNode; onChange: () => void;
}) {
  const prefix = useId();
  const notify = useToast();
  const [document, setDocument] = useState('');
  const [tokenType, setTokenType] = useState<'accessToken' | 'refreshToken'>('accessToken');
  const [pending, setPending] = useState('');
  const [feedback, setFeedback] = useState<Feedback | null>(null);
  const busy = disabled || !!pending || !!policy?.active;
  const groupProps = (group: Group) => ({ id: `${prefix}-${group}` });
  const message = (group: Group) => feedback?.group === group
    ? <p className={feedback.error ? 'field-error' : 'account-feedback'} role={feedback.error ? 'alert' : 'status'}>{feedback.message}</p>
    : null;
  const run = async (name: string, path: string, body: unknown, success: string, method?: string) => {
    setPending(name); setFeedback(null);
    try {
      await api(token, `/accounts/${id}/${path}`, body, undefined, method);
      if (name === 'import') setDocument('');
      notify(success); onChange();
    } catch (error) {
      notify(error instanceof Error ? error.message : '操作失败', 'error'); onChange();
    } finally { setPending(''); }
  };
  const importDocument = () => {
    if (busy || !document.trim()) return;
    let parsed: unknown;
    try { parsed = document.trim().startsWith('{') ? JSON.parse(document) : { [tokenType]: document.trim() }; }
    catch { setFeedback({ group: 'credentials', message: 'JSON 格式无效，请检查导入内容。', error: true }); return; }
    void run('import', 'credentials', { document: parsed }, '凭据已保存。请在“接入与调度”中启用账户。');
  };
  const loginDescription = credentialSource === 'desktop' ? 'ChatGPT、设备码或本机登录' : 'ChatGPT 或设备码登录';

  return <div className="oauth-account account-groups">
    <AccountGroup {...groupProps('login')} title="登录方式" description={loginDescription} icon={<LogIn size={18} />}>
      <fieldset className="account-login-options" aria-label="选择登录方式" disabled={busy}>
        {loginActions}
        {credentialSource === 'desktop' ? <details className="account-extra"><summary>本机 ChatGPT 绑定详情</summary><p className="helper">此账户沿用原有 ChatGPT 登录绑定。桌面切换账户后可重新绑定。</p><button type="button" className="button secondary full" disabled={busy} onClick={() => void run('desktop', 'credentials/desktop', {}, '已绑定本机登录。请在“接入与调度”中启用接入。')}><Monitor size={16} aria-hidden="true" />{pending === 'desktop' ? '正在读取本机登录…' : '复用本机 ChatGPT 登录'}</button></details> : null}
      </fieldset>
      <p className="helper">登录或导入的凭据只保存在此独立账户中。</p>
      {loginResult}
      {message('login')}
    </AccountGroup>

    <AccountGroup {...groupProps('credentials')} title="凭据管理" description={credentialSource ? credentialNames[credentialSource] : '尚未配置 · 可导入已有凭据'} icon={<KeyRound size={18} />}>
      <div className="account-field">
        <label htmlFor={`${prefix}-file`}>选择凭据文件</label>
        <input id={`${prefix}-file`} type="file" accept=".json,application/json" disabled={busy} aria-describedby={`${prefix}-file-help`} onChange={event => {
          const file = event.target.files?.[0]; event.target.value = ''; if (!file) return;
          if (file.size > 262144) { setFeedback({ group: 'credentials', message: '凭据文件不能超过 256 KiB。', error: true }); return; }
          void file.text().then(value => { setDocument(value); setFeedback(null); }).catch(() => setFeedback({ group: 'credentials', message: '无法读取文件。', error: true }));
        }} />
        <p id={`${prefix}-file-help`} className="helper">支持 JSON 文件，最大 256 KiB。</p>
      </div>
      <div className="account-field">
        <label htmlFor={`${prefix}-token-type`}>纯文本令牌类型</label>
        <select id={`${prefix}-token-type`} value={tokenType} disabled={busy} onChange={event => { setTokenType(event.target.value as 'accessToken' | 'refreshToken'); setFeedback(null); }}>
          <option value="accessToken">Access Token</option>
          <option value="refreshToken">Refresh Token</option>
        </select>
        <p id={`${prefix}-token-help`} className="helper">JSON 按字段识别，无需选择类型。单独 RT 会先联网补全凭据，再交由原生后端自动续期。</p>
      </div>
      <div className="account-field">
        <label htmlFor={`${prefix}-document`}>或粘贴 JSON / {tokenType === 'refreshToken' ? 'Refresh Token' : 'Access Token'}</label>
        <textarea id={`${prefix}-document`} value={document} onChange={event => setDocument(event.target.value)} rows={3} maxLength={262144} spellCheck={false} autoComplete="off" disabled={busy} aria-describedby={`${prefix}-token-help`} />
      </div>
      <button type="button" className="button secondary full" disabled={busy || !document.trim()} onClick={importDocument}><Upload size={16} aria-hidden="true" />{pending === 'import' ? '正在导入…' : '导入到独立实例'}</button>
      <details className="account-extra"><summary>支持格式与续期说明</summary><p className="helper">支持官方 auth.json、完整凭据 JSON、单独 AT 或 Codex Refresh Token。RT 也可使用 refresh_token / refreshToken 字段导入；成功补全后自动续期。若交换结果缺少 ID Token，则无法导入。AT-only 到期后需重新导入。凭据仅保存于此实例的受限目录。</p></details>
      <AccountCredentialExport id={id} token={token} source={credentialSource} disabled={busy || !!policy?.queued || !!policy?.deleting} />
      <div className="account-credential-exit">
        <button type="button" className="text-button" disabled={busy} onClick={() => void run('logout', 'credentials', {}, '实例凭据已清除，本机桌面登录不受影响。', 'DELETE')}><Unplug size={15} aria-hidden="true" />退出此实例 / 解除绑定</button>
        <p className="helper">仅清除此实例凭据，不会退出本机桌面登录。</p>
      </div>
      {message('credentials')}
    </AccountGroup>

  </div>;
}
