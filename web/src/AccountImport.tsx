import { useId, useRef, useState } from 'react';
import { Upload } from 'lucide-react';
import { api } from './api';
import type { AccountBatchResult, Operations } from './api';
import { accountBackupMaxBytes, missingBackupGroups, parseAccountBackup } from './accountBackup';
import type { BackupPreview } from './accountBackup';
import { Modal } from './ui';
import { useToast } from './Toast';
import './account-transfer.css';

const reasons: Record<string, string> = {
  DUPLICATE_ACCOUNT: '账户身份已存在，已跳过',
  BUSY: '账户忙碌，已跳过',
  CONFIG_CONFLICT: '配置已变化，请刷新后检查剩余账户',
  SERVICE_DRAINING: '服务正在退出',
  INVALID_INPUT: '账户数量、分组或配置不符合限制',
  OAUTH_EXPIRED: '凭据已过期，请重新登录',
};
type ImportResult = { name: string; status: string; reason: string };

export function AccountImport({
  token,
  operations,
  disabled,
  reload,
}: {
  token: string;
  operations: Operations;
  disabled: boolean;
  reload: () => Promise<void>;
}) {
  const prefix = useId();
  const notify = useToast();
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [text, setText] = useState('');
  const [preview, setPreview] = useState<BackupPreview | null>(null);
  const [groupId, setGroupId] = useState('');
  const [error, setError] = useState('');
  const [result, setResult] = useState<ImportResult[] | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const inFlight = useRef(false);
  const errorBox = useRef<HTMLParagraphElement>(null);
  const report = (message: string) => {
    setError(message);
    requestAnimationFrame(() => errorBox.current?.focus());
  };
  const close = () => {
    if (inFlight.current) return;
    setOpen(false);
    setText('');
    setPreview(null);
    setResult(null);
  };
  const readFile = async (file: File) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setPending(true);
    setError('');
    setText('');
    setPreview(null);
    try {
      if (file.size > accountBackupMaxBytes) throw new Error('账户备份不能超过 4 MiB。');
      const content = await file.text();
      setText(content);
      setPreview(parseAccountBackup(content));
    } catch (cause) {
      report(cause instanceof Error ? cause.message : '文件读取失败，请重新选择。');
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };
  const inspect = () => {
    try {
      setPreview(parseAccountBackup(text));
      setError('');
    } catch (cause) {
      setPreview(null);
      report(cause instanceof Error ? cause.message : '备份格式无效。');
    }
  };
  const missingGroups = preview ? missingBackupGroups(preview.items, operations.groups) : false;
  const groupInvalid = groupId
    ? !operations.groups.some((group) => group.id === groupId)
    : missingGroups;
  const submit = async () => {
    if (!preview || disabled || inFlight.current || result || uncertain) return;
    if (groupInvalid) {
      report('原分组不存在或备份未包含分组标识，请选择目标分组。');
      return;
    }
    const body = {
      revision: operations.revision,
      action: 'import',
      items: preview.items,
      ...(groupId ? { groupIds: [groupId] } : {}),
    };
    if (new Blob([JSON.stringify(body)]).size > accountBackupMaxBytes) {
      report('本次导入请求超过 4 MiB，请拆分备份后导入。');
      return;
    }
    inFlight.current = true;
    setPending(true);
    setError('');
    try {
      const response = await api<AccountBatchResult>(token, '/accounts/batch', body);
      const rows: ImportResult[] = response.items.map((item, index) => ({
        name: preview.items[index]?.name ?? `第 ${index + 1} 项`,
        status: item.status,
        reason:
          item.status === 'succeeded'
            ? preview.items[index]?.source === 'api-key'
              ? '已导入上游配置和密钥，可直接启用'
              : preview.items[index]?.document
                ? '已导入，账户保持停用'
                : '已导入配置，需要重新连接'
            : (reasons[item.code ?? ''] ?? '未导入，请检查凭据或账户状态'),
      }));
      setResult(rows);
      setText('');
      setPreview(null);
      const succeeded = rows.filter((item) => item.status === 'succeeded').length;
      const failed = rows.filter((item) => item.status === 'failed').length;
      const skipped = rows.filter((item) => item.status === 'skipped').length;
      notify(
        `账户导入完成：成功 ${succeeded}，失败 ${failed}，跳过 ${skipped}。`,
        failed ? 'error' : skipped ? 'info' : 'success',
      );
      try {
        await reload();
      } catch {
        report('导入结果已返回，但账户列表刷新失败，请关闭窗口后刷新列表。');
      }
    } catch (cause) {
      if (cause instanceof TypeError) {
        setUncertain(true);
        setText('');
        setPreview(null);
        report(
          '连接中断，导入可能仍在执行。请关闭窗口并刷新账户列表，核对结果后再处理未导入的账户。',
        );
      } else report(cause instanceof Error ? cause.message : '导入失败，请检查备份后重试。');
      try {
        await reload();
      } catch {
        /* Keep the original import error visible. */
      }
    } finally {
      inFlight.current = false;
      setPending(false);
    }
  };
  return (
    <>
      <button
        type="button"
        className="button secondary"
        disabled={disabled || pending}
        aria-haspopup="dialog"
        onClick={() => {
          setOpen(true);
          setText('');
          setPreview(null);
          setGroupId('');
          setError('');
          setResult(null);
          setUncertain(false);
        }}
      >
        <Upload size={16} aria-hidden="true" />
        导入账户备份
      </button>
      {open ? (
        <Modal title="导入账户备份" busy={pending} close={close} trapFocus>
          <div className="account-transfer-form" aria-busy={pending}>
            <p className="helper">
              恢复列表中“导出账户备份”生成的 JSON，每批 1–32 项，最大 4
              MiB。保留名称、分组和并发；新账户保持停用。独立凭据按身份去重，逐项导入，失败不会撤销已成功的账户。
            </p>
            {error ? (
              <p ref={errorBox} tabIndex={-1} className="field-error" role="alert">
                {error}
              </p>
            ) : null}
            {!result && !uncertain ? (
              <>
                <label htmlFor={`${prefix}-file`}>账户备份文件</label>
                <input
                  id={`${prefix}-file`}
                  type="file"
                  accept=".json,application/json"
                  disabled={pending}
                  onChange={(event) => {
                    const file = event.target.files?.[0];
                    event.target.value = '';
                    if (file) void readFile(file);
                  }}
                />
                <label htmlFor={`${prefix}-json`}>或粘贴账户备份 JSON</label>
                <textarea
                  id={`${prefix}-json`}
                  rows={5}
                  value={text}
                  spellCheck={false}
                  autoComplete="off"
                  disabled={pending}
                  onChange={(event) => {
                    setText(event.target.value);
                    setPreview(null);
                    setError('');
                  }}
                />
                <button
                  type="button"
                  className="button secondary"
                  disabled={pending || !text.trim()}
                  onClick={inspect}
                >
                  预览备份
                </button>
                {preview ? (
                  <section className="account-import-preview" aria-label="备份预览">
                    <h3>可导入 {preview.items.length} 个账户</h3>
                    <ul>
                      {preview.items.map((item, index) => (
                        <li key={index}>
                          <strong>{item.name}</strong>
                          <span>
                            {item.source === 'api-key'
                              ? 'API Key 配置 · 包含上游密钥'
                              : item.document
                                ? '包含独立凭据'
                                : '仅配置 · 需要重新连接'}{' '}
                            · 并发 {item.maxConcurrency}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </section>
                ) : null}
                <label htmlFor={`${prefix}-group`}>导入分组</label>
                <select
                  id={`${prefix}-group`}
                  value={groupId}
                  disabled={pending}
                  aria-describedby={`${prefix}-group-help`}
                  onChange={(event) => {
                    setGroupId(event.target.value);
                    setError('');
                  }}
                >
                  <option value="">保留备份中的原分组</option>
                  {operations.groups.map((group) => (
                    <option key={group.id} value={group.id}>
                      {group.name}
                      {group.enabled ? '' : '（已暂停）'}
                    </option>
                  ))}
                </select>
                <p id={`${prefix}-group-help`} className={groupInvalid ? 'field-error' : 'helper'}>
                  {groupInvalid
                    ? '原分组不存在或缺少分组标识，请选择目标分组后导入。'
                    : groupId
                      ? '本次新账户统一加入所选分组；已有账户保持不变。'
                      : '保留全部原分组；迁移到其他服务时可统一选择目标分组。'}
                </p>
                <div className="row-actions">
                  <button
                    type="button"
                    className="button secondary"
                    disabled={pending}
                    onClick={close}
                  >
                    取消
                  </button>
                  <button
                    type="button"
                    className="button primary"
                    disabled={disabled || pending || !preview || groupInvalid}
                    onClick={() => void submit()}
                  >
                    {pending ? '正在处理…' : '确认导入账户'}
                  </button>
                </div>
              </>
            ) : (
              <>
                {result ? (
                  <section className="account-import-preview" aria-label="导入结果">
                    <h3>导入结果</h3>
                    <ul>
                      {result.map((item, index) => (
                        <li key={index}>
                          <strong>{item.name}</strong>
                          <span>
                            {item.status === 'succeeded'
                              ? '成功'
                              : item.status === 'skipped'
                                ? '跳过'
                                : '失败'}{' '}
                            · {item.reason}
                          </span>
                        </li>
                      ))}
                    </ul>
                    <p className="helper">
                      请在账户详情检查接入后启用账户。包含凭据或上游密钥的账户可直接启用；仅配置账户需重新连接，重复导入仅配置记录会新建账户。
                    </p>
                  </section>
                ) : null}
                <button type="button" className="button primary" disabled={pending} onClick={close}>
                  完成并关闭
                </button>
              </>
            )}
          </div>
        </Modal>
      ) : null}
    </>
  );
}
