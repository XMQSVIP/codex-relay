import { useEffect, useState } from 'react';
import { FileText, LoaderCircle } from 'lucide-react';
import { api } from './api';
import type { DebugSettings } from '../contracts/admin';

export function RequestDebug({ token }: { token: string }) {
  const [settings, setSettings] = useState<DebugSettings | null>(null);
  const [error, setError] = useState('');
  const [pending, setPending] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    void api<DebugSettings>(token, '/settings/debug', undefined, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) {
          setSettings(value);
          setError('');
        }
      })
      .catch((cause) => {
        if (!controller.signal.aborted)
          setError(cause instanceof Error ? cause.message : '加载失败');
      });
    return () => controller.abort();
  }, [token, attempt]);
  const toggle = async () => {
    if (!settings || pending) return;
    setPending(true);
    setError('');
    try {
      setSettings(
        await api<DebugSettings>(token, '/settings/debug', {
          enabled: !settings.enabled,
          revision: settings.revision,
        }),
      );
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '保存失败');
      setAttempt((value) => value + 1);
    } finally {
      setPending(false);
    }
  };
  return (
    <section
      className="panel request-debug-settings"
      aria-labelledby="request-debug-title"
      aria-busy={pending}
    >
      <header className="environment-tool-heading">
        <FileText size={18} aria-hidden="true" />
        <h2 id="request-debug-title">请求原文调试</h2>
      </header>
      <p className="helper">保存新请求与响应的原文，用于排查对话和工具调用。</p>
      <div className="request-debug-control">
        <div>
          <strong>
            {pending
              ? '正在保存…'
              : !settings
                ? error
                  ? '设置加载失败'
                  : '正在加载…'
                : settings.enabled
                  ? '原文记录已开启'
                  : '原文记录已关闭'}
          </strong>
          <span>{settings?.enabled ? '关闭时清空已保存的原文' : '仅在需要排查时开启'}</span>
        </div>
        <button
          type="button"
          className="request-debug-switch"
          role="switch"
          aria-checked={settings?.enabled ?? false}
          aria-label="请求原文调试"
          aria-describedby="request-debug-policy"
          disabled={!settings}
          aria-disabled={pending}
          onClick={() => void toggle()}
        >
          <span className="request-debug-track">
            <span>
              {pending ? <LoaderCircle size={12} className="spin" aria-hidden="true" /> : null}
            </span>
          </span>
        </button>
      </div>
      <p className="request-debug-policy" id="request-debug-policy">
        关闭或重启服务会清空原文，保留请求统计。原文包含对话、工具参数和附件；每侧最多 32
        MiB，超出时标记截断。
      </p>
      <a className="text-button request-debug-link" href="?view=requests">
        前往请求监控查看原文
      </a>
      {error ? (
        <p role="alert" className="field-error">
          {error}{' '}
          <button
            type="button"
            className="text-button"
            onClick={() => setAttempt((value) => value + 1)}
          >
            重新加载
          </button>
        </p>
      ) : null}
    </section>
  );
}
