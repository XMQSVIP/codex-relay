import { useState } from 'react';
import { Check, CircleAlert, LoaderCircle, RefreshCw, Terminal } from 'lucide-react';
import { api } from './api';
import type { Overview } from './api';

const pathFields = [
  ['appServerExecutable', 'Codex 路径'],
  ['desktopExecutable', '桌面程序路径'],
  ['archive', '应用归档路径（app.asar）'],
] as const;

export function InstallationSettings({
  token,
  data,
  disabled,
  reload,
}: {
  token: string;
  data: Overview;
  disabled: boolean;
  reload: () => void;
}) {
  const [codexPath, setCodexPath] = useState(data.installation?.appServerExecutable ?? '');
  const [standalone, setStandalone] = useState(data.installation?.manuallyConfigured ?? false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const [acceptedId, setAcceptedId] = useState<string | null>(null);
  const installation = data.installation;
  const preparation = data.jobs.filter((job) => job.kind === 'prepare').at(-1);
  const accepted = data.jobs.find((job) => job.id === acceptedId);
  const waiting = accepted?.state === 'running' || (!!acceptedId && !accepted && data.busy);
  const busy = disabled || submitting || waiting || data.busy || preparation?.state === 'running';
  const failure = error || (!submitting && !waiting ? preparation?.error?.message : '');

  const prepare = async () => {
    if (busy) return;
    setSubmitting(true);
    setError('');
    setAcceptedId(null);
    try {
      const result = await api<{ id: string }>(token, '/jobs', {
        kind: 'prepare',
        ...(standalone ? { paths: { appServerExecutable: codexPath.trim() } } : {}),
      });
      setAcceptedId(result.id);
      reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '检测未能开始，请重试。');
    } finally {
      setSubmitting(false);
    }
  };

  const validating = submitting || waiting || preparation?.state === 'running';
  return (
    <section className="panel runtime-settings" aria-labelledby="installation-title">
      <header className="environment-section-heading">
        <span className="environment-section-icon">
          <Terminal size={20} aria-hidden="true" />
        </span>
        <div>
          <h2 id="installation-title">原生运行环境</h2>
          <p>中转站启动原生进程时使用的 Codex 安装。</p>
        </div>
      </header>
      <div className={`runtime-current ${installation ? 'ready' : 'unconfigured'}`}>
        <div className="runtime-current-heading">
          <span className={`badge ${installation ? 'success' : 'warning'}`}>
            {installation ? (
              <Check size={14} aria-hidden="true" />
            ) : (
              <CircleAlert size={14} aria-hidden="true" />
            )}
            {installation ? '已配置' : '待配置'}
          </span>
          <span>
            {installation
              ? `当前生效 · ${installation.manuallyConfigured ? '独立模式' : '自动模式'}`
              : '尚未准备原生环境'}
          </span>
          {installation?.appServerVersion ? (
            <code className="runtime-version">{installation.appServerVersion}</code>
          ) : null}
        </div>
        {installation ? (
          <dl className="runtime-paths">
            {pathFields
              .filter(
                ([key]) => installation[key] && (!standalone || key === 'appServerExecutable'),
              )
              .map(([key, label]) => (
                <div key={key}>
                  <dt>{label}</dt>
                  <dd>
                    <code>{installation[key]}</code>
                  </dd>
                </div>
              ))}
          </dl>
        ) : (
          <p className="helper">选择下方运行模式并检测安装，完成后即可为账户启动原生进程。</p>
        )}
      </div>
      <form
        className="runtime-form"
        onSubmit={(event) => {
          event.preventDefault();
          void prepare();
        }}
      >
        <div className="runtime-mode-row">
          <div className="account-field">
            <label htmlFor="installation-mode">运行模式</label>
            <select
              id="installation-mode"
              value={standalone ? 'standalone' : 'automatic'}
              disabled={busy}
              aria-describedby="installation-mode-help"
              onChange={(event) => {
                setStandalone(event.target.value === 'standalone');
                setError('');
              }}
            >
              <option value="automatic">自动模式</option>
              <option value="standalone">独立模式</option>
            </select>
          </div>
          <p id="installation-mode-help" className="helper">
            {standalone
              ? '使用指定的 Codex 可执行文件，无需安装桌面应用。'
              : '自动识别正在运行的官方 Codex 桌面及其 Codex 路径。'}
          </p>
        </div>
        {standalone ? (
          <div className="account-field runtime-path-field">
            <label htmlFor="installation-codex">Codex 路径</label>
            <input
              id="installation-codex"
              value={codexPath}
              onChange={(event) => setCodexPath(event.target.value)}
              required
              disabled={busy}
              spellCheck={false}
              autoComplete="off"
              aria-describedby="installation-path-help"
            />
            <p id="installation-path-help" className="helper">
              填写中转站所在机器上的绝对路径，无需引号。Windows 为 codex.exe，macOS/Linux 为
              codex。校验通过后保存，新启动的进程使用新路径。
            </p>
          </div>
        ) : null}
        <div className="runtime-form-footer">
          <p className="helper">
            {standalone
              ? '仅校验版本与协议连接，不发送模型请求。账户登录需单独完成。'
              : '检测通过后保存安装信息。检测时请保持官方桌面运行。'}
          </p>
          <button
            className="button primary"
            type="submit"
            disabled={busy || (standalone && !codexPath.trim())}
          >
            {validating ? (
              <LoaderCircle size={16} className="spin" aria-hidden="true" />
            ) : standalone ? (
              <Check size={16} aria-hidden="true" />
            ) : (
              <RefreshCw size={16} aria-hidden="true" />
            )}
            {standalone ? '校验并保存路径' : installation ? '重新检测安装' : '检测安装'}
          </button>
        </div>
        {validating ? (
          <p className="runtime-feedback helper" role="status">
            正在校验原生安装…
          </p>
        ) : null}
        {accepted?.state === 'completed' ? (
          <p className="runtime-feedback runtime-saved" role="status">
            安装路径已校验并保存。
          </p>
        ) : null}
        {failure ? (
          <p className="field-error" role="alert">
            {failure} 请检查路径或安装状态后重试。
          </p>
        ) : null}
      </form>
    </section>
  );
}
