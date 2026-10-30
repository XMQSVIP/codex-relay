import { useToast } from './Toast';
import { useEffect, useId, useRef, useState } from 'react';
import { X } from 'lucide-react';
import { ApiError } from './api';
import type { ClientKey, Group } from './api';
import { KeyFastMode } from './KeyFastMode';
import './key-editor.css';

export const validKeyConcurrency = (value: number) => Number.isInteger(value) && value >= 0 && value <= 256;
export type KeyEditValues = { name: string; groupId: string; maxConcurrency: number; fastMode: boolean };

export function KeyEditor({ value, groups, disabled, save, close }: {
  value: ClientKey; groups: Group[]; disabled: boolean; save: (values: KeyEditValues) => Promise<void>; close: () => void;
}) {
  const id = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const nameField = useRef<HTMLInputElement>(null);
  const submitting = useRef(false);
  const [name, setName] = useState(value.name);
  const [groupId, setGroup] = useState(value.groupId);
  const [limit, setLimit] = useState(value.maxConcurrency);
  const [fastMode, setFastMode] = useState(value.fastMode ?? false);
  const notify = useToast();
  const [conflict, setConflict] = useState(false);
  const [pending, setPending] = useState(false);
  useEffect(() => {
    const previous = document.body.style.overflow; document.body.style.overflow = 'hidden';
    dialog.current?.showModal(); nameField.current?.focus();
    return () => { document.body.style.overflow = previous; };
  }, []);
  const valid = !!name.trim() && [...name.trim()].length <= 60 && !/[\u0000-\u001f\u007f]/.test(name) && validKeyConcurrency(limit) && groups.some(group => group.id === groupId && (group.enabled || group.id === value.groupId));
  const dirty = name.trim() !== value.name || groupId !== value.groupId || limit !== value.maxConcurrency || fastMode !== (value.fastMode ?? false);
  const dismiss = () => { dialog.current?.close(); close(); };
  const submit = async () => {
    if (submitting.current || disabled || !valid || !dirty || conflict) return;
    submitting.current = true; setPending(true);
    try { await save({ name: name.trim(), groupId, maxConcurrency: limit, fastMode }); dismiss(); }
    catch (cause) { notify(cause instanceof TypeError ? '无法确认保存结果，请检查连接并重新打开编辑窗口。' : cause instanceof Error ? cause.message : '保存失败，请重试。', 'error'); if (cause instanceof ApiError && cause.code === 'CONFIG_CONFLICT') setConflict(true); }
    finally { submitting.current = false; setPending(false); }
  };
  return <dialog ref={dialog} className="key-create-dialog key-edit-dialog" aria-labelledby={`${id}-heading`} aria-describedby={`${id}-description`} onCancel={event => { event.preventDefault(); if (!pending) dismiss(); }}>
    <div className="section-heading"><div><h2 id={`${id}-heading`}>编辑 API Key</h2><p id={`${id}-description`}>修改名称、所属分组、调用方并发与快速模式。</p></div><button type="button" className="button secondary key-dialog-close" aria-label="关闭编辑窗口" disabled={pending} onClick={dismiss}><X size={18} aria-hidden="true" /></button></div>
    <form className="key-form" aria-busy={pending} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <div className="key-edit-field">
        <label htmlFor={`${id}-name`}>密钥名称</label><input ref={nameField} id={`${id}-name`} required maxLength={60} value={name} onChange={event => setName(event.target.value)} disabled={pending} />
      </div>
      <div className="key-edit-routing">
        <div className="key-edit-field">
          <label htmlFor={`${id}-group`}>所属分组</label><select id={`${id}-group`} value={groupId} onChange={event => setGroup(event.target.value)} disabled={pending} aria-describedby={`${id}-group-help`}>{groups.map(group => <option key={group.id} value={group.id} disabled={!group.enabled && group.id !== value.groupId}>{group.name}{group.enabled ? '' : '（已暂停）'}</option>)}</select>
          <p id={`${id}-group-help`} className="helper">新请求使用新分组；原分组的排队请求会失去授权。</p>
        </div>
        <div className="key-edit-field">
          <label htmlFor={`${id}-limit`}>调用方并发上限</label><input id={`${id}-limit`} type="number" min={0} max={256} step={1} required value={Number.isNaN(limit) ? '' : limit} onChange={event => setLimit(event.target.valueAsNumber)} disabled={pending} aria-invalid={!validKeyConcurrency(limit)} aria-describedby={`${id}-limit-help`} />
          <p id={`${id}-limit-help`} className="helper">0 为不限，最高 256。超限排队，调低不影响已执行请求。</p>
        </div>
      </div>
      <dl className="key-edit-models"><dt>模型范围</dt><dd>{value.models.length ? value.models.join('、') : '不单独限制模型 ID'}</dd></dl>
      <KeyFastMode value={fastMode} disabled={pending} onChange={setFastMode} />

      <div className="key-dialog-actions"><button type="button" className="button secondary" disabled={pending} onClick={dismiss}>{conflict ? '关闭后重新编辑' : '取消'}</button><button type="submit" className="button primary" disabled={disabled || pending || !valid || !dirty || conflict}>{pending ? '正在保存…' : '保存修改'}</button></div>
    </form>
  </dialog>;
}
