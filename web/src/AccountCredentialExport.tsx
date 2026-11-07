import { useRef, useState } from 'react';
import { Download } from 'lucide-react';
import { api } from './api';
import type { Account, AccountBackup } from './api';
import { downloadAccountJson } from './accountBackup';
import { Modal } from './ui';
import { useToast } from './Toast';
import './account-transfer.css';

export function AccountCredentialExport({
  id,
  token,
  source,
  disabled,
}: {
  id: string;
  token: string;
  source: Account['credentialSource'];
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const inFlight = useRef(false);
  const notify = useToast();
  if (!source || source === 'desktop' || source === 'api-key') return null;
  const download = async () => {
    if (disabled || inFlight.current) return;
    inFlight.current = true;
    setPending(true);
    setError('');
    try {
      const backup = await api<AccountBackup>(token, '/accounts/export', { accountIds: [id] });
      const item = backup.items[0];
      if (!item?.document || item.requiresReconnect)
        throw new Error('此账户没有可导出的独立凭据，请重新登录或导入凭据。');
      downloadAccountJson(item.document, `codex-credentials-${id}.json`);
      setOpen(false);
      notify('单账户凭据已导出，可在“导入凭据”中直接使用。');
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '凭据导出失败，请稍后重试。');
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };
  return (
    <>
      <button
        type="button"
        className="button secondary full"
        disabled={disabled || pending}
        aria-haspopup="dialog"
        onClick={() => {
          setError('');
          setOpen(true);
        }}
      >
        <Download size={16} aria-hidden="true" />
        导出凭据
      </button>
      {open ? (
        <Modal title="导出单账户凭据" busy={pending} close={() => setOpen(false)} trapFocus>
          <div className="account-transfer-form">
            <p>
              下载此账户的凭据
              JSON，可直接在其他独立账户的“导入凭据”中使用；不包含名称、分组或并发配置。备份这些配置请使用账户列表的“导出账户备份”。
            </p>
            <p className="helper">
              文件包含登录凭据，请妥善保管。
              {source === 'external'
                ? '此账户仅有 Access Token，到期后仍需重新导入。'
                : '迁移后请停用旧实例，避免同一续期凭据被多处同时刷新。'}
            </p>
            {error ? (
              <p className="field-error" role="alert">
                {error}
              </p>
            ) : null}
            <div className="row-actions">
              <button
                type="button"
                className="button secondary"
                disabled={pending}
                onClick={() => setOpen(false)}
              >
                取消
              </button>
              <button
                type="button"
                className="button primary"
                disabled={disabled || pending}
                onClick={() => void download()}
              >
                {pending ? '正在导出…' : '确认下载凭据 JSON'}
              </button>
            </div>
          </div>
        </Modal>
      ) : null}
    </>
  );
}
