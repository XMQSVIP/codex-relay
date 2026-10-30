import { ChevronDown, Monitor, Play, Square } from 'lucide-react';
import type { Overview, Job } from './api';

export function DesktopDebug({
  desktop,
  ready,
  preparation,
  busy,
  prepare,
  start,
  stop,
  standalone,
}: {
  desktop: Overview['desktop'];
  ready: boolean;
  preparation?: Job;
  busy: boolean;
  prepare: () => void;
  start: () => void;
  stop: () => void;
  standalone?: boolean;
}) {
  const state = desktop.state;
  const running = ['starting', 'waiting-for-desktop', 'desktop-connected', 'stopping'].includes(
    state,
  );
  const label =
    (
      {
        starting: '正在启动',
        'waiting-for-desktop': '等待桌面连接',
        'desktop-connected': '桌面已连接',
        stopping: '正在停止',
        stopped: '未启动',
        failed: '启动或运行失败',
      } as Record<string, string>
    )[state] ?? '未启动';
  return (
    <details className="panel desktop-debug" open>
      <summary className="environment-tool-heading">
        <Monitor size={18} aria-hidden="true" />
        <h2>桌面连接调试</h2>
        <ChevronDown size={16} className="desktop-debug-chevron" aria-hidden="true" />
      </summary>
      <div className="desktop-copy">
        <div className="desktop-debug-state">
          <span
            className={`badge ${state === 'desktop-connected' ? 'success' : state === 'failed' ? 'warning' : 'neutral'}`}
          >
            <span className="status-dot" aria-hidden="true" />
            {label}
          </span>
          <span>本机桌面连接</span>
        </div>
        <p>
          {standalone
            ? '当前为独立 Codex 模式，桌面连接调试不可用。账户登录与客户端 API 不依赖桌面连接。'
            : '排查官方桌面与原生后端的连接。此状态仅用于桌面调试，不代表客户端 API 可用。'}
        </p>
        <div className="desktop-debug-actions">
          <button
            className="button secondary"
            disabled={busy || !ready || standalone || running}
            onClick={start}
          >
            <Play size={16} aria-hidden="true" />
            启动桌面调试
          </button>
          <button
            className="button secondary"
            disabled={
              busy || !['starting', 'waiting-for-desktop', 'desktop-connected'].includes(state)
            }
            onClick={stop}
          >
            <Square size={16} aria-hidden="true" />
            停止桌面调试
          </button>
        </div>
        {desktop.error ? (
          <p className="field-error" role="alert">
            {desktop.error.message}
          </p>
        ) : null}
        {preparation ? (
          <p className={preparation.error ? 'field-error' : 'small-note'} role="status">
            安装检测：
            {preparation.error?.message ??
              (preparation.state === 'running'
                ? '正在检测…'
                : ready
                  ? '已完成。'
                  : '请查看上方诊断结果。')}
          </p>
        ) : null}
        <details className="desktop-debug-guide">
          <summary>
            <ChevronDown size={15} aria-hidden="true" />
            启动前与停止后须知
          </summary>
          <p>先保持官方桌面运行并检测安装，再保存工作、退出普通桌面，最后启动调试连接。</p>
          <button className="text-button" disabled={busy || running} onClick={prepare}>
            检测原生安装
          </button>
          <p>
            停止会断开调试连接并回收本服务启动的后端进程，桌面窗口保留。再次启动前，请先退出该窗口。客户端
            API 使用“客户端接入”中的本机配置实例或独立 OAuth 账户。
          </p>
        </details>
      </div>
    </details>
  );
}
