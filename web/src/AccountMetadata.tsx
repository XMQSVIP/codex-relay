import { useId, useRef, useState } from 'react';
import { Pencil } from 'lucide-react';
import { api } from './api';
import type { Account, AccountMetadata as Metadata } from './api';
import { Modal } from './ui';
import { useToast } from './Toast';

export function AccountMetadata({
  account,
  token,
  disabled,
  reload,
}: {
  account: Account;
  token: string;
  disabled: boolean;
  reload: () => void;
}) {
  const inputId = useId();
  const input = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState('');
  const [revision, setRevision] = useState(0);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const notify = useToast();
  const reset = () => {
    setNote(account.note);
    setRevision(account.metadataRevision);
    setError('');
  };
  const save = async () => {
    if (disabled || pending) return;
    setPending(true);
    setError('');
    try {
      await api<Metadata>(token, `/accounts/${account.id}/metadata`, {
        metadataRevision: revision,
        note: note.trim(),
      });
      setOpen(false);
      notify('账户备注已保存。');
      reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '保存失败，请重试。');
      reload();
    } finally {
      setPending(false);
    }
  };
  const defaultName =
    account.email?.split('@')[0] ||
    (account.source === 'native-config'
      ? '本机 Codex'
      : account.source === 'api-key'
        ? 'API Key 账户'
        : '未命名账户');
  const busy = disabled || pending;
  return (
    <>
      <button
        type="button"
        className="text-button account-note-trigger"
        aria-label="编辑账户备注"
        aria-haspopup="dialog"
        disabled={disabled}
        onClick={() => {
          reset();
          setOpen(true);
        }}
      >
        <Pencil size={14} aria-hidden="true" />
        编辑备注
      </button>
      {open ? (
        <Modal
          title="编辑账户备注"
          busy={pending}
          initialFocus={input}
          close={() => setOpen(false)}
        >
          <form
            className="account-note-form"
            onSubmit={(event) => {
              event.preventDefault();
              void save();
            }}
            aria-busy={pending}
          >
            <div className="account-field">
              <label htmlFor={inputId}>账户备注（可选）</label>
              <input
                id={inputId}
                value={note}
                maxLength={500}
                ref={input}
                disabled={busy}
                autoComplete="off"
                placeholder={defaultName}
                aria-describedby={`${inputId}-help`}
                onChange={(event) => setNote(event.target.value)}
              />
              <p className="helper" id={`${inputId}-help`}>
                备注会作为账户名称。留空时显示“{defaultName}”。
              </p>
            </div>
            {error ? (
              <p className="field-error" role="alert">
                {error}
              </p>
            ) : null}
            <div className="account-dialog-actions">
              <button type="button" className="button secondary" disabled={busy} onClick={reset}>
                还原备注
              </button>
              <button
                type="button"
                className="button secondary"
                disabled={pending}
                onClick={() => setOpen(false)}
              >
                取消
              </button>
              <button className="button primary" disabled={busy}>
                {pending ? '正在保存…' : '保存备注'}
              </button>
            </div>
          </form>
        </Modal>
      ) : null}
    </>
  );
}
