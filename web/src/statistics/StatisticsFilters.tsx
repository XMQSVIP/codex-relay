import { useId, useRef } from 'react';
import { ChevronDown, Settings2 } from 'lucide-react';
import type { Operations } from '../api';
import { accountGroupIds } from '../accountMembership';
import { requestKinds, requestStatuses } from '../RequestRecords';
import type { StatisticsExtraFilters } from './useStatisticsData';

type BasicFilter = 'groupId' | 'accountId' | 'kind' | 'status';
type Filters = StatisticsExtraFilters & Record<BasicFilter, string>;

export function StatisticsFilters({
  operations: ops,
  filters,
  id,
  open,
  onClose,
  onClear,
  onFilterChange,
  onExtraChange,
}: {
  operations: Operations;
  filters: Filters;
  id: string;
  open: boolean;
  onClose: () => void;
  onClear: () => void;
  onFilterChange: (field: BasicFilter, value: string) => void;
  onExtraChange: (value: StatisticsExtraFilters) => void;
}) {
  const { groupId, accountId, kind, status, ...extra } = filters;
  const { keyId, model, errorCode, keyName, accountName, groupName } = extra;
  const advancedForm = useRef<HTMLFormElement>(null);
  const hasFilters = Object.values(filters).some(Boolean);
  const clearFilters = () => {
    onClear();
    advancedForm.current?.reset();
  };
  return (
    <section
      className="panel statistics-filters"
      id={id}
      aria-label="统计筛选"
      hidden={!open}
      onKeyDown={(event) => {
        if (event.key === 'Escape') {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="section-heading">
        <div>
          <h2>筛选条件</h2>
          <p>同时应用到指标、趋势、排行与记录；实时容量保持全局值。</p>
        </div>
        {hasFilters ? (
          <button type="button" className="text-button" onClick={clearFilters}>
            清除筛选
          </button>
        ) : null}
      </div>
      <div className="record-filters">
        <Filter
          label="分组"
          value={groupId}
          set={(v) => {
            onFilterChange('groupId', v);
          }}
          options={ops.groups.map((g) => [g.id, g.name])}
        />
        <Filter
          label="账户"
          value={accountId}
          set={(v) => {
            onFilterChange('accountId', v);
          }}
          options={ops.accounts
            .filter((a) => !groupId || accountGroupIds(a).includes(groupId))
            .map((a) => [a.id, a.name])}
        />
        <Filter
          label="请求类型"
          value={kind}
          set={(v) => onFilterChange('kind', v)}
          options={Object.entries(requestKinds)}
        />
        <Filter
          label="请求状态"
          value={status}
          set={(v) => onFilterChange('status', v)}
          options={Object.entries(requestStatuses)}
        />
      </div>
      <details className="record-advanced">
        <summary>
          <Settings2 size={14} aria-hidden="true" />
          更多筛选
          {Object.values(extra).some(Boolean) ? (
            <span className="count">{Object.values(extra).filter(Boolean).length} 项已应用</span>
          ) : (
            <span>名称关键词、Key ID、模型、错误码</span>
          )}
          <ChevronDown size={14} aria-hidden="true" />
        </summary>
        <form
          key={`${keyId}:${model}:${errorCode}:${accountName}:${groupName}`}
          ref={advancedForm}
          className="record-filters"
          onSubmit={(e) => {
            e.preventDefault();
            const values = new FormData(e.currentTarget);
            onExtraChange({
              keyName,
              keyId: String(values.get('keyId') ?? '').trim(),
              model: String(values.get('model') ?? '').trim(),
              errorCode: String(values.get('errorCode') ?? '').trim(),
              accountName: String(values.get('accountName') ?? '').trim(),
              groupName: String(values.get('groupName') ?? '').trim(),
            });
          }}
        >
          <label>
            账户名称关键词
            <input defaultValue={accountName} name="accountName" maxLength={128} />
          </label>
          <label>
            分组名称关键词
            <input defaultValue={groupName} name="groupName" maxLength={128} />
          </label>
          <label>
            Key ID
            <input
              defaultValue={keyId}
              name="keyId"
              maxLength={36}
              autoComplete="off"
              spellCheck={false}
              placeholder="完整 Key ID"
            />
          </label>
          <label>
            模型
            <input
              defaultValue={model}
              name="model"
              maxLength={128}
              autoComplete="off"
              spellCheck={false}
              placeholder="完整模型 ID"
            />
          </label>
          <label>
            错误码
            <input
              defaultValue={errorCode}
              name="errorCode"
              maxLength={128}
              autoComplete="off"
              spellCheck={false}
              placeholder="例如 RPC_CLOSED"
            />
          </label>
          <button className="button secondary" type="submit">
            应用筛选
          </button>
        </form>
      </details>
    </section>
  );
}

function Filter({
  label,
  value,
  options,
  set,
}: {
  label: string;
  value: string;
  options: string[][];
  set: (v: string) => void;
}) {
  const id = useId();
  return (
    <div>
      <label htmlFor={id}>{label}</label>
      <select id={id} value={value} onChange={(e) => set(e.target.value)}>
        <option value="">全部{label}</option>
        {options.map(([v, text]) => (
          <option value={v} key={v}>
            {text}
          </option>
        ))}
      </select>
    </div>
  );
}
