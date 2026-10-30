import { useId, useState } from 'react';
import type { ReactNode } from 'react';
import { ArrowRight, ChevronDown, Clock3, Gauge, Link2, Save } from 'lucide-react';
import type { Operations } from './api';

export function ConnectionSettings({ operations: ops, disabled, save, children }: {
  operations: Operations;
  disabled: boolean;
  save: (path: string, body: object, message: string) => Promise<boolean>;
  children: ReactNode;
}) {
  return <section className="panel connection-settings" aria-labelledby="connection-settings-title">
    <div className="connection-settings-header">
      <div><h2 id="connection-settings-title">连接与调度</h2><p className="helper">管理请求容量、等待队列与对话的账户复用。</p></div>
      <a className="text-button" href="?view=keys">设置 API Key 并发<ArrowRight size={14} aria-hidden="true" /></a>
    </div>
    <div className="connection-settings-sections">
      <section className="connection-setting-card" aria-labelledby="capacity-title">
        <div className="connection-setting-description">
          <h3 id="capacity-title"><Gauge size={17} aria-hidden="true" />服务总并发</h3>
          <p className="helper">自动汇总账户容量，或手动指定服务上限。新账户默认 5 并发。</p>
          <div className="connection-setting-status"><span><strong>{ops.active}</strong> / {ops.globalLimit}</span><span>执行中</span></div>
          <progress className="connection-setting-meter" value={Math.min(ops.active, ops.globalLimit)} max={Math.max(1, ops.globalLimit)} aria-label={`服务总并发：${ops.active} 个执行中，上限 ${ops.globalLimit}`} />
        </div>
        <GlobalLimit operations={ops} disabled={disabled} save={values => save('/settings/concurrency', values, '服务总并发已保存，新请求将按新上限调度。')} />
      </section>
      <QueueSettings operations={ops} disabled={disabled} save={(queueLimit, queueTimeoutSeconds) => save('/settings/queue', { queueLimit, queueTimeoutSeconds }, '等待队列已保存，新入队请求将使用新设置。')} />
      <AffinitySettings operations={ops} disabled={disabled} save={nativeAffinityTtlHours => save('/settings/native-affinity', { nativeAffinityTtlHours }, '账户亲和时间已保存，将用于新绑定及下一次续期。')} />
      {children}
    </div>
    <details className="connection-settings-rules">
      <summary><ChevronDown size={14} aria-hidden="true" />并发与排队如何生效</summary>
      <ul>
        <li>自动模式按所有账户配置的并发上限求和，添加、删除账户或修改账户并发后自动调整；手动模式保持指定总上限。停用账户仍保留配置容量，但不会接收客户端请求。</li>
        <li>请求同时受服务总并发、账户并发与 API Key 并发限制；新账户默认 5 并发，可在账户详情中修改。</li>
        <li>并发满额后进入队列；有容量时按入队顺序调度，受某个账户或 Key 限制的请求不会阻塞其他可执行请求。</li>
        <li>降低并发不打断正在执行的请求，缩短队列不移除已有等待请求；超时设置仅对新入队请求生效。</li>
        <li>队列满时返回 429，等待超时返回 504；暂停账户或分组会拒绝新请求并结束对应排队。</li>
      </ul>
    </details>
  </section>;
}

function AffinitySettings({ operations: ops, disabled, save }: { operations: Operations; disabled: boolean; save: (hours: number) => Promise<boolean> }) {
  const [draft, setDraft] = useState<number | null>(null);
  const hours = draft ?? ops.nativeAffinityTtlHours;
  const valid = Number.isInteger(hours) && hours >= 1 && hours <= 720;
  const id = useId();
  const [pending, setPending] = useState(false);
  const submit = async () => {
    if (disabled || pending || !valid || hours === ops.nativeAffinityTtlHours) return;
    setPending(true);
    try { if (await save(hours)) setDraft(null); }
    finally { setPending(false); }
  };
  return <section className="connection-setting-card" aria-labelledby={`${id}-title`}>
    <div className="connection-setting-description">
      <h3 id={`${id}-title`}><Link2 size={17} aria-hidden="true" />账户亲和</h3>
      <p className="helper">原生请求在同一对话中优先复用原账户。成功准入时续期，临时分流也保留并续期原账户绑定。</p>
    </div>
    <form className="connection-settings-form" aria-busy={pending} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <div><label htmlFor={id}>账户亲和时间</label><div className="connection-number"><input id={id} type="number" required step={1} min={1} max={720} value={Number.isNaN(hours) ? '' : hours} onChange={event => setDraft(event.target.valueAsNumber)} disabled={disabled || pending} aria-invalid={!valid} aria-describedby={`${id}-help`} /><span>小时</span></div></div>
      <div className="connection-settings-footer">
        <p id={`${id}-help`} className={valid ? 'helper' : 'field-error'}>默认 24 小时，可设置 1–720 个整数小时。修改后用于新绑定及下一次续期；此时间不代表上游 Token 缓存保留时间。</p>
        <button type="submit" className="button secondary" disabled={disabled || pending || !valid || hours === ops.nativeAffinityTtlHours}><Save size={15} aria-hidden="true" />{pending ? '正在保存…' : '保存亲和时间'}</button>
      </div>
    </form>
  </section>;
}

function GlobalLimit({ operations: ops, disabled, save }: { operations: Operations; disabled: boolean; save: (value: { globalLimitAuto: boolean; globalLimit?: number }) => Promise<boolean> }) {
  const [draft, setDraft] = useState<{ automatic: boolean; limit: number } | null>(null);
  const automatic = draft?.automatic ?? ops.globalLimitAuto;
  const limit = draft?.limit ?? ops.manualGlobalLimit;
  const id = useId();
  const valid = automatic || Number.isInteger(limit) && limit >= 1 && limit <= 256;
  const dirty = automatic !== ops.globalLimitAuto || !automatic && limit !== ops.manualGlobalLimit;
  const submit = async () => {
    if (!valid || !dirty || disabled) return;
    if (await save(automatic ? { globalLimitAuto: true } : { globalLimitAuto: false, globalLimit: limit })) setDraft(null);
  };
  return <form className="connection-settings-form" onSubmit={event => { event.preventDefault(); void submit(); }}>
    <div><label htmlFor={`${id}-mode`}>总并发模式</label><select id={`${id}-mode`} value={automatic ? 'auto' : 'manual'} onChange={event => setDraft({ automatic: event.target.value === 'auto', limit })} disabled={disabled} aria-describedby={`${id}-help`}><option value="auto">自动</option><option value="manual">手动</option></select></div>
    {!automatic ? <div><label htmlFor={id}>同时执行上限</label><div className="connection-number"><input id={id} type="number" min={1} max={256} step={1} required value={Number.isNaN(limit) ? '' : limit} onChange={event => setDraft({ automatic, limit: event.target.valueAsNumber })} disabled={disabled} aria-describedby={`${id}-help`} /><span>个请求</span></div></div> : null}
    <div className="connection-settings-footer">
      <p id={`${id}-help`} className="helper">{automatic ? `按账户并发配置自动汇总，当前为 ${ops.accountCapacity} 个请求。两个默认账户共 10 并发。` : '1–256 个请求，所有账户共享此上限。添加账户后保持此值。'}</p>
      <button className="button secondary" disabled={disabled || !valid || !dirty}><Save size={15} aria-hidden="true" />保存并发</button>
    </div>
  </form>;
}

function QueueSettings({ operations: ops, disabled, save }: { operations: Operations; disabled: boolean; save: (length: number, seconds: number) => Promise<boolean> }) {
  const [length, setLength] = useState(ops.queueLimit);
  const [seconds, setSeconds] = useState(ops.queueTimeoutSeconds);
  const id = useId();
  const valid = Number.isInteger(length) && length >= 1 && length <= 4096 && Number.isInteger(seconds) && seconds >= 1 && seconds <= 300;
  return <section className="connection-setting-card" aria-labelledby="queue-settings-title">
    <div className="connection-setting-description">
      <h3 id="queue-settings-title"><Clock3 size={17} aria-hidden="true" />等待队列</h3>
      <p className="helper">并发满额时暂存请求，有可用容量后按入队顺序调度。</p>
      <div className="connection-setting-status"><span><strong>{ops.queued}</strong> / {ops.queueLimit}</span><span>排队中</span></div>
      <progress className="connection-setting-meter" value={Math.min(ops.queued, ops.queueLimit)} max={Math.max(1, ops.queueLimit)} aria-label={`等待队列：${ops.queued} 个排队中，上限 ${ops.queueLimit}`} />
    </div>
    <form className="connection-settings-form" onSubmit={event => { event.preventDefault(); if (valid && !disabled) void save(length, seconds); }}>
      <div><label htmlFor={`${id}-length`}>最多等待请求数</label><input id={`${id}-length`} type="number" min={1} max={4096} step={1} required value={Number.isNaN(length) ? '' : length} onChange={event => setLength(event.target.valueAsNumber)} disabled={disabled} aria-describedby={`${id}-help`} /></div>
      <div><label htmlFor={`${id}-seconds`}>等待超时（秒）</label><input id={`${id}-seconds`} type="number" min={1} max={300} step={1} required value={Number.isNaN(seconds) ? '' : seconds} onChange={event => setSeconds(event.target.valueAsNumber)} disabled={disabled} aria-describedby={`${id}-help`} /></div>
      <div className="connection-settings-footer">
        <p id={`${id}-help`} className="helper">最多等待 1–4096 个请求，超时范围为 1–300 秒。</p>
        <button className="button secondary" disabled={disabled || !valid || length === ops.queueLimit && seconds === ops.queueTimeoutSeconds}><Save size={15} aria-hidden="true" />保存队列</button>
      </div>
    </form>
  </section>;
}
