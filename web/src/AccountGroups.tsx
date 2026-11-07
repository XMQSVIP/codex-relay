import { useState } from 'react';
import type { Group } from './api';
import { Modal } from './ui';
import { sameGroups } from './accountMembership';
import './account-groups.css';

export function GroupChoices({ groups, value, onChange, disabled, label = '所属分组' }: {
  groups: Group[]; value: string[]; onChange: (value: string[]) => void; disabled?: boolean; label?: string;
}) {
  return <fieldset className="account-group-choices" disabled={disabled}>
    <legend>{label}</legend>
    <p className="helper">可选择多个分组，至少保留一个。账户并发上限由所有分组共享。</p>
    <div className="account-group-options">{groups.map(group => <label key={group.id}>
      <input type="checkbox" checked={value.includes(group.id)} disabled={value.length === 1 && value.includes(group.id)} onChange={event => onChange(event.target.checked ? [...value, group.id] : value.filter(id => id !== group.id))} />
      <span>{group.name}{group.enabled ? '' : '（已暂停）'}</span>
    </label>)}</div>
  </fieldset>;
}

export function AccountGroupControl({ name, groups, value, revision, disabled, busy, save }: {
  name: string; groups: Group[]; value: string[]; revision: number; disabled: boolean; busy: boolean;
  save: (value: string[], revision: number) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState<{ ids: string[]; revision: number } | null>(null);
  const names = groups.filter(group => value.includes(group.id)).map(group => group.name).join('、');
  return <>
    <button type="button" className="account-group-trigger" aria-label={`${name}的所属分组`} aria-haspopup="dialog" aria-expanded={draft !== null} title={names} disabled={disabled} onClick={() => setDraft({ ids: [...value], revision })}>{names}<span>编辑</span></button>
    {draft ? <Modal title={`设置分组：${name}`} busy={busy} close={() => setDraft(null)}>
      <form onSubmit={event => { event.preventDefault(); void save(draft.ids, draft.revision).then(saved => { if (saved) setDraft(null); }); }}>
        <GroupChoices groups={groups} value={draft.ids} onChange={ids => setDraft({ ...draft, ids })} disabled={disabled || busy} />
        <div className="row-actions"><button className="button primary" disabled={disabled || busy || !draft.ids.length || sameGroups(value, draft.ids)}>{busy ? '正在保存…' : '保存分组'}</button><button type="button" className="button secondary" disabled={busy} onClick={() => setDraft(null)}>取消</button></div>
      </form>
    </Modal> : null}
  </>;
}
