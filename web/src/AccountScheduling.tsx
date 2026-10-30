import { useEffect, useId, useState } from 'react';
import { LoaderCircle, Save } from 'lucide-react';
import type { AccountPolicy } from './api';

export function AccountScheduling({
  account,
  disabled,
  pending,
  cancel,
  save,
  additionalDirty = false,
  cancelLabel = '收起',
  onDirtyChange,
}: {
  account: AccountPolicy;
  disabled: boolean;
  pending: boolean;
  additionalDirty?: boolean;
  cancelLabel?: string;
  onDirtyChange?: (dirty: boolean) => void;
  cancel: () => void;
  save: (values: { maxConcurrency: number }) => Promise<boolean>;
}) {
  const id = useId();
  const [maxConcurrency, setLimit] = useState(account.maxConcurrency);
  const limit = maxConcurrency;
  const valid = Number.isInteger(limit) && limit >= 1 && limit <= 256;
  const dirty = additionalDirty || limit !== account.maxConcurrency;
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);
  return (
    <form
      className="account-scheduling-form"
      aria-label={`${account.name}的调度设置`}
      onSubmit={(event) => {
        event.preventDefault();
        if (valid && !disabled) void save({ maxConcurrency: limit });
      }}
    >
      <div className="account-scheduling-limit">
        <label htmlFor={`${id}-limit`}>账户并发上限</label>
        <input
          id={`${id}-limit`}
          type="number"
          min={1}
          max={256}
          step={1}
          required
          value={Number.isNaN(limit) ? '' : limit}
          onChange={(event) => setLimit(event.target.valueAsNumber)}
          disabled={disabled}
          aria-describedby={`${id}-help`}
        />
        <p id={`${id}-help`} className="helper">
          新账户默认 5，可设置 1–256
          个同时执行的请求。自动模式会同步调整服务总并发；请求仍受密钥上限限制。降低上限不会中断正在执行的请求。
        </p>
      </div>
      <div className="account-scheduling-actions">
        <button type="button" className="button secondary" disabled={disabled} onClick={cancel}>
          {cancelLabel}
        </button>
        <button className="button primary" disabled={disabled || !valid || !dirty}>
          {pending ? (
            <LoaderCircle size={15} className="spin" aria-hidden="true" />
          ) : (
            <Save size={15} aria-hidden="true" />
          )}
          {pending ? '正在保存' : '保存调度'}
        </button>
      </div>
    </form>
  );
}
