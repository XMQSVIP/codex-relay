import { LoaderCircle } from 'lucide-react';
import type { Account } from './api';
import { hasCredentials } from './accountStatus';

export function AccountActivationHelp({ account, configured, installationReady }: {
  account: Account; configured: boolean; installationReady: boolean;
}) {
  if (account.source === 'api-key' && !account.upstream?.hasApiKey) return <p className="helper">尚未配置上游密钥。<a className="account-action-hint" href={`?view=accounts&account=${encodeURIComponent(account.id)}&section=login`}>补填 API 密钥</a></p>;
  if (configured || account.source === 'api-key' || account.source === 'shared-desktop') return null;
  return <>
    {!hasCredentials(account) ? <p className="helper">尚未配置登录凭据。<a className="account-action-hint" href={`?view=accounts&account=${encodeURIComponent(account.id)}&section=login`}>登录或导入凭据</a></p> : null}
    {!installationReady ? <p className="helper">启用时将自动检测原生安装，请保持官方 Codex 桌面运行。<a className="account-action-hint" href="?view=settings&tab=diagnostics">查看运行环境</a></p> : null}
  </>;
}

export function AccountActivationSwitch({ name, enabled, pending, disabled, describedBy, onChange }: {
  name: string; enabled: boolean; pending: 'enable' | 'disable' | null; disabled: boolean; describedBy?: string;
  onChange: (enabled: boolean) => void;
}) {
  return <button type="button" role="switch" aria-label={`启用账户：${name}`} aria-checked={enabled}
    aria-busy={!!pending} aria-describedby={describedBy} className="account-activation-switch" disabled={disabled || !!pending}
    onClick={() => onChange(!enabled)}>
    <span className="account-switch-track" aria-hidden="true"><span className="account-switch-thumb">{pending ? <LoaderCircle size={12} className="spin" /> : null}</span></span>
    <span>{pending === 'enable' ? '启用中…' : pending === 'disable' ? '停用中…' : '启用账户'}</span>
  </button>;
}
