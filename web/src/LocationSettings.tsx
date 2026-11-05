import { useEffect, useId, useState } from 'react';
import { Clock, RefreshCw, Save } from 'lucide-react';
import { api } from './api';
import type { AccountLocation, LocationStatus, Operations, ProxyStatus } from './api';
import './location-settings.css';

export const defaultLocation: AccountLocation = { enabled: false, source: 'proxy', custom: { timezone: '', country: '', region: '', city: '' } };
type Options = { timezones: string[]; countries: string[] };
type DetectionPreview = { revision: number; preview: { id: string; location: AccountLocation['custom']; detectedAt: number } };
const labels = { disabled: '已关闭', pending: '等待检测', detecting: '正在检测出口位置…', applied: '已生效', cached: '使用上次检测结果', failed: '检测失败，保留原请求行为' };

export function LocationSettings({ id: accountId, token, value, status, restart, revision, disabled, reload }: {
  id: string; token: string; value: AccountLocation; status?: LocationStatus; restart?: ProxyStatus;
  revision: number; disabled: boolean; reload: () => void;
}) {
  const id = useId();
  const [draft, setDraft] = useState({ config: value, revision });
  const [editing, setEditing] = useState(false);
  const [pending, setPending] = useState('');
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [preview, setPreview] = useState<DetectionPreview | null>(null);
  const [options, setOptions] = useState<Options>({ timezones: [], countries: [] });
  useEffect(() => { if (!editing) setDraft({ config: value, revision }); }, [value, revision, editing]);
  useEffect(() => {
    if (!draft.config.enabled || draft.config.source !== 'custom') return;
    let active = true;
    api<Options>(token, '/location/options').then(result => { if (active) setOptions(result); }, () => { if (active) setError('无法加载时区和国家列表，请刷新后重试。'); });
    return () => { active = false; };
  }, [draft.config.enabled, draft.config.source, token]);
  const config = draft.config;
  const locked = disabled || !!pending || restart?.state === 'switching';
  const dirty = JSON.stringify(config) !== JSON.stringify(value);
  const proxyLocation = config.enabled && config.source === 'proxy';
  const validPreview = preview?.revision === revision ? preview : null;
  const detecting = pending === 'detect' || status?.state === 'detecting';
  const statusText = pending === 'save' ? '正在保存设置…'
    : detecting ? labels.detecting
    : restart?.state === 'switching' ? '正在立即重启账户实例…'
    : restart?.state === 'failed' ? '重启失败，账户暂不接收新任务。'
    : proxyLocation && validPreview ? '检测完成，尚未保存'
    : dirty ? (proxyLocation ? '待检测，尚未保存' : '有未保存的更改')
    : labels[status?.state ?? (value.enabled ? 'pending' : 'disabled')];
  const update = (next: Partial<AccountLocation>) => {
    setEditing(true); setDraft(current => ({ ...current, config: { ...current.config, ...next } })); setPreview(null); setError(''); setMessage('');
  };
  const custom = (key: keyof AccountLocation['custom'], text: string) => update({ custom: { ...config.custom, [key]: text } });
  const validation = () => {
    if (!config.enabled || config.source !== 'custom') return '';
    if (!options.timezones.includes(config.custom.timezone.trim())) return '请选择有效的 IANA 时区，例如 America/New_York。';
    if (!config.custom.country && (config.custom.city.trim() || config.custom.region.trim())) return '填写地区或城市时，请先选择国家。';
    return '';
  };
  const save = async (retry = false) => {
    if (locked) return;
    if (!retry && proxyLocation && !validPreview) { setError('请先检测时区，确认预览结果后再保存。'); return; }
    const invalid = retry ? '' : validation();
    if (invalid) { setError(invalid); return; }
    setPending('save'); setError(''); setMessage('');
    try {
      const result = await api<Operations>(token, `/accounts/${accountId}/location`, { revision: retry ? revision : proxyLocation ? validPreview!.revision : draft.revision, config: retry ? value : config, ...(retry ? { retry: true } : proxyLocation ? { previewId: validPreview!.preview.id } : {}) });
      if (!retry) { setDraft({ config: result.accounts.find(a => a.id === accountId)?.location ?? config, revision: result.revision }); setEditing(false); }
      else { setDraft(current => ({ ...current, revision: result.revision })); }
      setPreview(null); setMessage('设置已保存。'); reload();
    } catch (cause) { setError(cause instanceof Error ? cause.message : '保存失败。'); reload(); }
    finally { setPending(''); }
  };
  const detect = async () => {
    if (locked || detecting || !proxyLocation) return;
    setPending('detect'); setPreview(null); setError(''); setMessage('');
    try { setPreview(await api<DetectionPreview>(token, `/accounts/${accountId}/location/detect`, { revision })); }
    catch (cause) { setError(cause instanceof Error ? cause.message : '检测失败。'); }
    finally { setPending(''); }
  };
  const countryNames = new Intl.DisplayNames(['zh-CN'], { type: 'region' });
  const effective = status?.effective;
  return <section className="panel detail-form" id="account-location">
    <div className="section-heading"><h2><Clock size={17} aria-hidden="true" />时区与搜索位置</h2><span className="settings-state" role="status" aria-live="polite">{statusText}</span></div>
    <form aria-label="时区与搜索位置设置" onSubmit={event => { event.preventDefault(); void save(); }}>
      <label className="location-enable" htmlFor={`${id}-enabled`}><input id={`${id}-enabled`} type="checkbox" checked={config.enabled} disabled={locked} onChange={event => update({ enabled: event.target.checked })} />启用时区与搜索位置覆盖</label>
      {config.enabled ? <>
        <label htmlFor={`${id}-source`}>位置来源</label>
        <select id={`${id}-source`} value={config.source} disabled={locked} onChange={event => update({ source: event.target.value as AccountLocation['source'] })}><option value="proxy">跟随账号代理</option><option value="custom">自定义</option></select>
        {config.source === 'custom' ? <div className="location-fields">
          <div><label htmlFor={`${id}-timezone`}>时区</label><input id={`${id}-timezone`} list={`${id}-timezones`} value={config.custom.timezone} required maxLength={128} placeholder="搜索，例如 Asia/Shanghai" disabled={locked} aria-describedby={`${id}-error`} onBlur={() => setError(validation())} onChange={event => custom('timezone', event.target.value)} /><datalist id={`${id}-timezones`}>{options.timezones.map(tz => <option key={tz} value={tz} />)}</datalist></div>
          <div><label htmlFor={`${id}-country`}>国家或地区（选填）</label><select id={`${id}-country`} value={config.custom.country} disabled={locked} onChange={event => custom('country', event.target.value)}><option value="">不指定</option>{options.countries.map(code => <option key={code} value={code}>{countryNames.of(code)}（{code}）</option>)}</select></div>
          <div><label htmlFor={`${id}-region`}>省／州（选填）</label><input id={`${id}-region`} value={config.custom.region} maxLength={128} disabled={locked} onChange={event => custom('region', event.target.value)} /></div>
          <div><label htmlFor={`${id}-city`}>城市（选填）</label><input id={`${id}-city`} value={config.custom.city} maxLength={128} disabled={locked} onChange={event => custom('city', event.target.value)} /></div>
        </div> : <><p className="helper" id={`${id}-detect-help`}>先检测并预览时区与位置，确认后再保存。检测不会修改设置或重启实例，不发送账号凭据。</p><details className="location-detection-help"><summary>检测方式与分流设置</summary><p>优先通过 ChatGPT 诊断端点获取出口 IP，再用 IPinfo 查询该 IP 的时区；失败时回退 IPWhois。已取得出口 IP 时，备用服务仍查询同一 IP。诊断端点不可用时，备用结果可能受分流影响，请确认后保存。</p></details></>}
      </> : <p className="helper location-disabled-help">启用后可跟随代理出口，或手动指定时区与搜索位置。关闭时沿用原生默认设置。</p>}
      {proxyLocation && validPreview ? <div className="location-preview" role="status" aria-label="检测预览"><strong>检测预览 · 尚未保存</strong><p>{[validPreview.preview.location.country ? countryNames.of(validPreview.preview.location.country) : '', validPreview.preview.location.region, validPreview.preview.location.city, validPreview.preview.location.timezone].filter(Boolean).join(' · ')}</p><p className="helper">确认后点击“保存检测结果”；预览保留 5 分钟。也可以放弃本次结果，保留现有设置。</p><button className="text-button" type="button" disabled={locked} onClick={() => { setPreview(null); setMessage('已放弃检测结果，现有设置未改变。'); }}>放弃检测结果</button></div> : null}
      {proxyLocation && preview && !validPreview ? <p className="helper">配置已变化，请重新检测后再保存。</p> : null}
      {effective && value.enabled ? <p className="helper">当前生效：{[effective.country ? countryNames.of(effective.country) : '', effective.region, effective.city, effective.timezone].filter(Boolean).join(' · ')}{status?.detectedAt ? ` · 检测时间：${new Date(status.detectedAt).toLocaleString()}` : ''}</p> : null}
      {status?.error && value.enabled && value.source === 'proxy' ? <p className="field-error" role="alert">{status.error}{status.effective ? ' 正在沿用同一代理的缓存。' : ' 当前请求不覆盖位置。'}</p> : null}
      {restart?.state === 'failed' ? <div role="alert"><p className="field-error">{restart.error?.message ?? '实例重启失败。'}</p><button className="button secondary" type="button" disabled={locked} onClick={() => void save(true)}>重试应用位置设置</button></div> : null}
      <p id={`${id}-error`} className="field-error" role={error ? 'alert' : undefined}>{error}</p>
      {message ? <p role="status">{message}</p> : null}
      <div className="settings-card-footer">
      <p className="proxy-help">用于对话日期、时区与搜索位置，不会开启搜索。保存变更会重启运行中的实例，中断当前请求、会话和登录；仅检测不会重启。</p>
      <div className="settings-card-actions">
        {editing ? <button className="text-button" type="button" disabled={locked} onClick={() => { setDraft({ config: value, revision }); setEditing(false); setPreview(null); setError(''); setMessage(''); }}>还原时区与位置</button> : null}
        {proxyLocation ? <button className="button secondary" type="button" disabled={locked || detecting} aria-describedby={`${id}-detect-help`} onClick={() => void detect()}><RefreshCw size={15} aria-hidden="true" />{pending === 'detect' ? '正在检测…' : preview ? '重新检测' : '检测时区'}</button> : null}
        <button className="button secondary" type="submit" disabled={locked || (proxyLocation ? !validPreview : !dirty)}><Save size={15} aria-hidden="true" />{pending === 'save' ? '正在保存…' : proxyLocation ? '保存检测结果' : '保存时区与位置'}</button>
      </div>
      </div>
    </form>
  </section>;
}
