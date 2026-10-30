import { useId } from 'react';

export function KeyFastMode({ value, disabled, onChange }: { value: boolean; disabled: boolean; onChange: (value: boolean) => void }) {
  const id = useId();
  return <div className="key-fast-setting">
    <label className="checkbox-label"><input type="checkbox" role="switch" checked={value} disabled={disabled} aria-describedby={`${id}-help`} onChange={event => onChange(event.target.checked)} />强制快速模式</label>
    <p className="helper" id={`${id}-help`}>开启后，此密钥的新模型请求使用快速模式；关闭时跟随客户端设置。</p>
  </div>;
}
