import { useState } from 'react';
import { History, Save } from 'lucide-react';
import './api-key-transfer.css';

export function RequestRetention({ value, disabled, save }: {
  value: number; disabled: boolean;
  save: (path: string, body: object, message: string) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState<number | null>(null);
  const days = draft ?? value;
  const valid = Number.isInteger(days) && days >= 1 && days <= 365;
  const [pending, setPending] = useState(false);
  const submit = async () => {
    if (disabled || pending || !valid || days === value) return;
    setPending(true);
    try {
      if (await save('/settings/retention', { retentionDays: days }, `请求记录保存时长已设为 ${days} 天，过期记录已清理。`)) setDraft(null);
    } finally { setPending(false); }
  };
  return <section className="connection-setting-card" aria-labelledby="retention-title">
    <div className="connection-setting-description">
      <h3 id="retention-title"><History size={17} aria-hidden="true" />请求记录</h3>
      <p className="helper">默认保存 30 天。到期记录会自动清理，对应的请求统计与失败诊断内容也会移除。</p>
    </div>
    <form className="connection-settings-form" aria-busy={pending} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <div><label htmlFor="request-retention-days">请求记录保存时长</label><div className="connection-number"><input id="request-retention-days" type="number" required min={1} max={365} step={1} value={Number.isNaN(days) ? '' : days} onChange={event => setDraft(event.target.valueAsNumber)} disabled={disabled || pending} aria-invalid={!valid} aria-describedby="retention-help" /><span>天</span></div></div>
      <div className="connection-settings-footer">
        <p id="retention-help" className={valid ? 'helper' : 'field-error'}>可设置 1–365 天。保存后立即清理超期记录，清理后无法恢复；执行中和排队中的请求会保留。</p>
        <button type="submit" className="button secondary" disabled={disabled || pending || !valid || days === value}><Save size={15} aria-hidden="true" />{pending ? '正在保存…' : '保存时长'}</button>
      </div>
    </form>
  </section>;
}
