import { useState } from 'react';
import { Download, LoaderCircle, Play, RefreshCw, Trash2, X } from 'lucide-react';
import { api, type ManagedCodex } from './api';

type Action = 'check' | 'install' | 'activate' | 'remove';
const cancellable = new Set(['checking', 'downloading', 'verifying', 'extracting', 'starting']);
const mib = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(1)} MB`;

export function ManagedCodexSettings({
  token,
  runtime,
  disabled,
  globalBusy,
  reload,
}: {
  token: string;
  runtime: ManagedCodex;
  disabled: boolean;
  globalBusy: boolean;
  reload: () => void;
}) {
  const [submitting, setSubmitting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState('');
  const [deleteVersion, setDeleteVersion] = useState<string | null>(null);
  const operation = runtime.operation;
  const running = !!operation && operation.finishedAt === null;
  const busy = disabled || globalBusy || submitting || running;
  const current = !!runtime.latestVersion && runtime.latestVersion === runtime.activeVersion;
  const downloaded = runtime.versions.some((item) => item.version === runtime.latestVersion);

  async function perform(action: Action, version?: string) {
    if (busy) return;
    setSubmitting(true);
    setError('');
    try {
      await api<{ id: string }>(token, '/native-runtime/actions', {
        action,
        ...(version ? { version } : {}),
      });
      setDeleteVersion(null);
      reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '操作未能开始，请重试。');
    } finally {
      setSubmitting(false);
    }
  }

  async function cancel() {
    if (!operation || cancelling) return;
    setCancelling(true);
    setError('');
    try {
      await api(token, '/native-runtime/cancel', { id: operation.id });
      reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '取消失败，请重试。');
    } finally {
      setCancelling(false);
    }
  }

  return (
    <section className="managed-codex" aria-label="Codex 版本管理">
      <div className="managed-codex-heading">
        <div>
          <h3>官方 Codex · Linux {runtime.architecture === 'arm64' ? 'ARM64' : 'x64'}</h3>
          <p className="helper">
            每天自动检查稳定版，点击后下载、校验并启用。账户启用后自动启动原生进程。
          </p>
        </div>
        <span className="badge">
          {runtime.latestVersion ? `最新 ${runtime.latestVersion}` : '尚未检查版本'}
        </span>
      </div>
      <div className="managed-codex-toolbar">
        <button
          className="button"
          type="button"
          disabled={busy}
          onClick={() => void perform('check')}
        >
          <RefreshCw size={16} aria-hidden="true" />
          检查更新
        </button>
        <button
          className="button primary"
          type="button"
          disabled={busy || current}
          onClick={() =>
            void perform(downloaded ? 'activate' : 'install', runtime.latestVersion ?? undefined)
          }
        >
          {downloaded ? (
            <Play size={16} aria-hidden="true" />
          ) : (
            <Download size={16} aria-hidden="true" />
          )}
          {current
            ? '已是最新版本'
            : downloaded
              ? '启用最新版本'
              : runtime.activeVersion
                ? '下载并启用新版'
                : '安装并启用 Codex'}
        </button>
        <span className="helper">
          {runtime.checkedAt
            ? `上次检查：${new Date(runtime.checkedAt).toLocaleString()}`
            : '尚无成功检查记录'}
        </span>
      </div>
      {submitting || running ? (
        <div className="managed-codex-progress">
          <div className="managed-codex-progress-heading">
            <p role="status">
              <LoaderCircle size={16} className="spin" aria-hidden="true" />
              {operation?.message || '正在提交操作…'}
            </p>
            {running && cancellable.has(operation.phase) ? (
              <button
                className="button"
                type="button"
                disabled={disabled || cancelling}
                onClick={() => void cancel()}
              >
                <X size={16} aria-hidden="true" />
                {cancelling ? '正在取消…' : '取消操作'}
              </button>
            ) : null}
          </div>
          {operation?.phase === 'downloading' ? (
            <>
              <progress
                aria-label="Codex 下载进度"
                max={operation.totalBytes || 1}
                value={
                  operation.totalBytes
                    ? Math.min(operation.downloadedBytes, operation.totalBytes)
                    : undefined
                }
              />
              <p className="helper">
                已下载 {mib(operation.downloadedBytes)}
                {operation.totalBytes ? ` / ${mib(operation.totalBytes)}` : ''}
              </p>
            </>
          ) : null}
        </div>
      ) : operation && !runtime.error ? (
        <p className="runtime-feedback runtime-saved" role="status">
          {operation.message}
        </p>
      ) : null}
      {error || runtime.error ? (
        <p className="field-error" role="alert">
          {error || runtime.error?.message}
        </p>
      ) : null}
      {runtime.versions.length ? (
        <div className="managed-codex-versions">
          <h3>已安装版本</h3>
          <p className="helper">
            可启用任意已安装版本。当前版本和仍被进程使用的版本不能删除；其他旧版本由你手动删除。
          </p>
          <ul>
            {runtime.versions.map((item) => {
              const active = item.version === runtime.activeVersion;
              return (
                <li key={item.version}>
                  <div className="managed-codex-version-label">
                    <code>{item.version}</code>
                    {active ? (
                      <span className="badge success">当前启用</span>
                    ) : item.inUse ? (
                      <span className="badge warning">进程使用中</span>
                    ) : (
                      <span className="badge">已安装</span>
                    )}
                  </div>
                  <div className="managed-codex-version-actions">
                    <button
                      className="button"
                      type="button"
                      disabled={busy || active}
                      aria-label={`启用 Codex ${item.version}`}
                      onClick={() => void perform('activate', item.version)}
                    >
                      <Play size={15} aria-hidden="true" />
                      启用
                    </button>
                    {deleteVersion === item.version ? (
                      <>
                        <span className="helper">删除此版本的程序文件？</span>
                        <button
                          className="button danger"
                          type="button"
                          disabled={busy}
                          aria-label={`确认删除 Codex ${item.version}`}
                          onClick={() => void perform('remove', item.version)}
                        >
                          确认删除
                        </button>
                        <button
                          className="button"
                          type="button"
                          disabled={busy}
                          onClick={() => setDeleteVersion(null)}
                        >
                          取消
                        </button>
                      </>
                    ) : (
                      <button
                        className="button"
                        type="button"
                        disabled={busy || active || item.inUse}
                        aria-label={`删除 Codex ${item.version}`}
                        title={
                          active
                            ? '当前启用的版本不能删除'
                            : item.inUse
                              ? '请先停止使用此版本的账户实例'
                              : undefined
                        }
                        onClick={() => {
                          setDeleteVersion(item.version);
                          setError('');
                        }}
                      >
                        <Trash2 size={15} aria-hidden="true" />
                        删除
                      </button>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}
    </section>
  );
}
