import { useId, useState } from 'react';
import { Search, X } from 'lucide-react';

export interface RequestFilterField {
  key: string;
  label: string;
  value: string;
  options?: [string, string][];
  nameSearch?: { key: string; value: string };
  maxLength?: number;
  hidden?: boolean;
}

export function RequestFilters({ fields, apply, clear }: { fields: RequestFilterField[]; apply: (key: string, value: string) => void; clear: () => void }) {
  const [selected, setSelected] = useState(() => fields.find(field => (field.value || field.nameSearch?.value) && !field.hidden)?.key ?? 'keyName');
  const field = fields.find(item => item.key === selected) ?? fields[0];
  const active = fields.filter(item => item.value || item.nameSearch?.value);
  return <div className="request-filter-controls">
    <div className="request-filter-search">
      <select aria-label="筛选字段" value={field.key} onChange={event => setSelected(event.target.value)}>{fields.filter(item => !item.hidden).map(item => <option key={item.key} value={item.key}>{item.label}</option>)}</select>
      <FilterSearch key={`${field.key}:${field.value}:${field.nameSearch?.value ?? ''}`} field={field} apply={apply} />
    </div>
    {active.length ? <details className="request-filter-menu"><summary aria-label="已应用筛选">筛选 {active.length}</summary><div className="request-filter-applied">{active.map(item => <button className="badge" type="button" key={item.key} onClick={() => apply(item.key, '')} aria-label={`移除${item.label}筛选`}>{item.label}：{item.nameSearch?.value || (item.options?.find(([value]) => value === item.value)?.[1] ?? item.value)}<X size={13} aria-hidden="true" /></button>)}<button className="text-button" type="button" onClick={clear}>清除筛选</button></div></details> : null}
  </div>;
}

function FilterSearch({ field, apply }: { field: RequestFilterField; apply: (key: string, value: string) => void }) {
  const listId = useId();
  const [draft, setDraft] = useState(() => field.nameSearch?.value || (field.options?.find(([value]) => value === field.value)?.[1] ?? field.value));
  const [error, setError] = useState('');
  const validate = () => {
    const value = draft.trim();
    const invalid = value && field.options && !field.nameSearch && !field.options.some(([id, label]) => label === value || id === value);
    setError(invalid ? `请选择有效的${field.label}。` : '');
    return !invalid;
  };
  return <div className="request-filter-input"><form role="search" aria-label="筛选请求记录" onSubmit={event => {
    event.preventDefault();
    if (!validate()) return;
    const value = draft.trim();
    const option = field.options?.find(([id, label]) => label === value || id === value);
    const exactId = field.nameSearch && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
    apply(!option && !exactId && value && field.nameSearch ? field.nameSearch.key : field.key, option?.[0] ?? value);
  }}>
    <button type="submit" aria-label="搜索请求记录" title="搜索请求记录"><Search size={18} aria-hidden="true" /></button>
    <input type="search" aria-label={`${field.label}筛选内容`} placeholder={field.nameSearch ? '输入名称关键词或选择候选项' : '输入筛选内容'} value={draft} onChange={event => { setDraft(event.target.value); setError(''); }} onBlur={validate} aria-invalid={!!error} aria-describedby={error ? `${listId}-error` : undefined} list={field.options ? listId : undefined} maxLength={field.maxLength ?? 128} autoComplete="off" spellCheck={false} />
    {field.options ? <datalist id={listId}>{field.options.map(([value, label]) => <option key={value} value={label}>{value}</option>)}</datalist> : null}
  </form>{error ? <p id={`${listId}-error`} className="field-error" role="alert">{error}</p> : null}</div>;
}
