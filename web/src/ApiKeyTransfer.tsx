import { useRef, useState } from 'react';
import { Download, Upload } from 'lucide-react';
import { api } from './api';
import type { Group } from './api';
import { Modal } from './ui';
import { useToast } from './Toast';
import './api-key-transfer.css';

type KeyBackup = { version: number; keys: unknown[] };
const maxBytes = 8 * 1024 * 1024;

export function ApiKeyTransfer({ token, groups, disabled, busyChanged, imported }: {
  token: string; groups: Group[]; disabled: boolean;
  busyChanged: (busy: boolean) => void; imported: () => void;
}) {
  const notify = useToast();
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const submitting = useRef(false);
  const [text, setText] = useState('');
  const [groupId, setGroupId] = useState('');
  const [error, setError] = useState('');
  const errorBox = useRef<HTMLParagraphElement>(null);
  const report = (message: string) => {
    setError(message);
    requestAnimationFrame(() => errorBox.current?.focus());
  };
  const begin = () => { submitting.current = true; setPending(true); busyChanged(true); };
  const end = () => { submitting.current = false; setPending(false); busyChanged(false); };
  const exportAll = async () => {
    if (disabled || submitting.current) return;
    begin();
    try {
      const backup = await api<KeyBackup>(token, '/api-keys/export');
      const url = URL.createObjectURL(new Blob([JSON.stringify(backup, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a');
      link.href = url; link.download = `client-api-keys-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      notify(`已导出全部 ${backup.keys.length} 个密钥，文件含完整密钥，请妥善保管。`);
    } catch (cause) { notify(cause instanceof Error ? cause.message : '导出失败，请重试。', 'error'); }
    finally { end(); }
  };
  const readFile = async (file: File) => {
    if (submitting.current) return;
    begin(); setError(''); setText('');
    try {
      if (file.size > maxBytes) throw new Error('文件过大，请选择不超过 8 MiB 的 JSON 备份。');
      setText(await file.text());
    } catch (cause) { report(cause instanceof Error ? cause.message : '文件读取失败，请重新选择。'); }
    finally { end(); }
  };
  const importKeys = async () => {
    if (disabled || submitting.current || !text.trim()) return;
    let backup: KeyBackup;
    try {
      if (new Blob([text]).size > maxBytes) throw new Error('备份过大，每批最多 8 MiB。');
      let parsed: unknown;
      try { parsed = JSON.parse(text.replace(/^\uFEFF/, '')); }
      catch { throw new Error('JSON 格式无效，请选择本页面导出的备份文件，或检查粘贴内容。'); }
      if (!parsed || typeof parsed !== 'object' || !('version' in parsed) || parsed.version !== 1 || !('keys' in parsed) || !Array.isArray(parsed.keys) || !parsed.keys.length || parsed.keys.length > 1000) {
        throw new Error('请选择版本为 1 的密钥备份，每批支持 1–1000 个密钥。');
      }
      backup = parsed as KeyBackup;
    } catch (cause) { report(cause instanceof Error ? cause.message : '备份格式无效。'); return; }
    begin(); setError('');
    try {
      const result = await api<{ imported: number; skipped: number }>(token, '/api-keys/import', { backup, ...(groupId ? { groupId } : {}) });
      setOpen(false); setText(''); imported();
      notify(`已导入 ${result.imported} 个密钥，跳过 ${result.skipped} 个重复密钥。`);
    } catch (cause) {
      report(cause instanceof TypeError ? '连接中断，暂时无法确认导入结果。可重新导入，已有密钥会自动跳过。' : cause instanceof Error ? cause.message : '导入失败，请检查备份后重试。');
    } finally { end(); }
  };
  return <>
    <button type="button" className="button secondary" disabled={disabled || pending} onClick={() => void exportAll()} title="导出所有密钥，不受筛选和分页影响；文件含完整密钥"><Download size={17} aria-hidden="true" />全部导出</button>
    <button type="button" className="button secondary" disabled={disabled || pending} aria-haspopup="dialog" onClick={() => { setOpen(true); setText(''); setGroupId(''); setError(''); }}><Upload size={17} aria-hidden="true" />批量导入</button>
    {open ? <Modal title="批量导入 API 密钥" busy={pending} close={() => setOpen(false)} trapFocus>
      <form className="key-transfer-form" aria-busy={pending} onSubmit={event => { event.preventDefault(); void importKeys(); }}>
        <p className="helper">选择导出的 JSON 文件或粘贴备份内容。保留原密钥、状态、模型权限、并发和有效期；重复密钥自动跳过。校验失败时整批不写入。</p>
        {error ? <p ref={errorBox} tabIndex={-1} className="message error" role="alert">{error}</p> : null}
        <label htmlFor="key-import-file">密钥备份文件</label><input id="key-import-file" type="file" accept=".json,application/json" disabled={pending} onChange={event => { const file = event.target.files?.[0]; event.target.value = ''; if (file) void readFile(file); }} />
        <label htmlFor="key-import-json">JSON 备份内容</label><textarea id="key-import-json" rows={8} value={text} autoComplete="off" spellCheck={false} disabled={pending} onChange={event => { setText(event.target.value); setError(''); }} placeholder={'{"version": 1, "keys": […]}'} />
        <label htmlFor="key-import-group">导入分组</label><select id="key-import-group" value={groupId} disabled={pending} onChange={event => { setGroupId(event.target.value); setError(''); }} aria-describedby="key-import-group-help"><option value="">保留备份中的原分组</option>{groups.filter(group => group.enabled).map(group => <option key={group.id} value={group.id}>{group.name}</option>)}</select>
        <p id="key-import-group-help" className="helper">从其他服务迁移、原分组不存在时，可为本次新导入的密钥统一选择目标分组。已有密钥的设置不变。</p>
        <div className="key-dialog-actions"><button type="button" className="button secondary" disabled={pending} onClick={() => setOpen(false)}>取消</button><button type="submit" className="button primary" disabled={disabled || pending || !text.trim()}>{pending ? '正在处理…' : '导入密钥'}</button></div>
      </form>
    </Modal> : null}
  </>;
}
