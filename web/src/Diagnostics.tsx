import {
  Check,
  ChevronDown,
  FileSearch,
  History,
  LoaderCircle,
  Monitor,
  Play,
  ScanLine,
  ShieldCheck,
  X,
} from 'lucide-react';
import type { Job, Overview, Report } from './api';

const diagnosticItems = [
  {
    kind: 'inspect-provider',
    name: '读取提供方',
    icon: FileSearch,
    description: '确认本机配置使用的提供方、模型和服务地址。',
    action: '读取配置',
  },
  {
    kind: 'doctor',
    name: '桌面检查',
    icon: Monitor,
    description: '检查本机安装、桌面进程和调试连接。',
    action: '检查桌面',
  },
  {
    kind: 'shared',
    name: '桌面连接协议验证',
    icon: ShieldCheck,
    description: '排查桌面连接的认证、会话共享与权限确认。',
    action: '验证协议',
  },
];

function CheckedTime({ value }: { value?: number }) {
  return value ? (
    <time dateTime={new Date(value).toISOString()}>{new Date(value).toLocaleString()}</time>
  ) : (
    <span>时间未知</span>
  );
}

function DiagnosticResult({ report }: { report: Report }) {
  const checks = report.checks ?? [];
  const passed = checks.filter((check) => check.state === 'passed').length;
  return (
    <>
      {report.provider ? (
        <>
          <dl className="diagnostic-provider">
            <div>
              <dt>提供方</dt>
              <dd>{report.provider.id}</dd>
            </div>
            <div>
              <dt>默认模型</dt>
              <dd>{report.provider.model}</dd>
            </div>
            <div>
              <dt>服务地址</dt>
              <dd>
                <code>{report.provider.baseUrl}</code>
              </dd>
            </div>
          </dl>
          <p className="diagnostic-result-note">仅为配置读取结果，未发送模型测试请求。</p>
        </>
      ) : null}
      {typeof report.sharedDesktopVerified === 'boolean' ? (
        <p className="diagnostic-result-note">
          桌面调试连接：
          {report.sharedDesktopVerified ? '已验证' : '未验证；如需排查，可展开下方“桌面连接调试”。'}
        </p>
      ) : null}
      {checks.length ? (
        <details className="diagnostic-checks" open={passed !== checks.length}>
          <summary>
            <ChevronDown size={15} aria-hidden="true" />
            <span>查看检查明细</span>
            <span className={`badge ${passed === checks.length ? 'success' : 'warning'}`}>
              {passed} / {checks.length} 通过
            </span>
          </summary>
          <ul className="check-list">
            {checks.map((check) => (
              <li key={check.id}>
                <span className={`check-symbol ${check.state === 'passed' ? 'passed' : 'failed'}`}>
                  {check.state === 'passed' ? (
                    <Check size={15} aria-label="通过" />
                  ) : (
                    <X size={15} aria-label="未通过" />
                  )}
                </span>
                <div>
                  <strong>{checkNames[check.id] ?? check.id}</strong>
                  <p>{check.detail}</p>
                </div>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </>
  );
}

export function Diagnostics({
  data,
  disabled,
  pendingKind,
  startJob,
}: {
  data: Overview;
  disabled: boolean;
  pendingKind?: string;
  startJob: (kind: string) => void;
}) {
  const jobs = [...data.jobs].sort((a, b) => b.createdAt - a.createdAt);
  return (
    <section className="panel diagnostic-workspace" aria-labelledby="diagnostic-title">
      <header className="environment-section-heading">
        <span className="environment-section-icon">
          <ScanLine size={20} aria-hidden="true" />
        </span>
        <div>
          <h2 id="diagnostic-title">本机环境检查</h2>
          <p>选择需要排查的项目，结果会保留在对应检查下。</p>
        </div>
        <span className="diagnostic-on-demand">按需运行</span>
      </header>
      {diagnosticItems.map((item) => {
        const unavailable =
          !!data.installation &&
          !data.installation.desktopExecutable &&
          item.kind !== 'inspect-provider';
        const job = jobs.find((value) => value.kind === item.kind);
        const report = job?.report ?? data.reports[item.kind];
        const running =
          pendingKind === item.kind || job?.state === 'running' || job?.state === 'queued';
        const failed =
          job?.state === 'failed' || !!report?.checks?.some((check) => check.state !== 'passed');
        const status = unavailable
          ? '无需桌面诊断'
          : running
            ? '检查中'
            : failed
              ? '需要处理'
              : job?.state === 'completed'
                ? '已完成'
                : report
                  ? '已有保存结果'
                  : '尚未检查';
        const saved = report && !job?.report;
        return (
          <section
            key={item.kind}
            className="diagnostic-item"
            aria-label={item.name}
            aria-busy={running}
          >
            <div className="diagnostic-item-heading">
              <span className="diagnostic-item-icon">
                <item.icon size={20} aria-hidden="true" />
              </span>
              <div className="diagnostic-item-copy">
                <h3>{item.name}</h3>
                <p>{unavailable ? '独立 Codex 模式无需此项桌面诊断。' : item.description}</p>
              </div>
              <span
                role="status"
                aria-label={`${item.name}：${status}`}
                className={`badge diagnostic-item-status ${running ? 'blue' : failed && !unavailable ? 'warning' : job?.state === 'completed' && !unavailable ? 'success' : 'neutral'}`}
              >
                {status}
              </span>
              <button
                type="button"
                className="button secondary diagnostic-run"
                disabled={disabled || unavailable || running}
                onClick={() => startJob(item.kind)}
                aria-label={`${item.action}：${item.name}`}
              >
                {running ? (
                  <LoaderCircle className="spin" size={15} aria-hidden="true" />
                ) : (
                  <Play size={15} aria-hidden="true" />
                )}
                {running ? '检查中…' : item.action}
              </button>
            </div>
            {job?.error && !running ? (
              <p className="field-error diagnostic-error" role="alert">
                {job.error.message} 处理后可重新运行此项检查。
              </p>
            ) : null}
            {report ? (
              <details
                className="diagnostic-result"
                key={`${job?.id ?? ''}:${report.checkedAt ?? ''}`}
                open={failed || !!job?.report}
              >
                <summary className="diagnostic-result-meta">
                  <ChevronDown size={15} aria-hidden="true" />
                  <span>{running ? '查看上次结果' : saved ? '查看保存结果' : '查看本次结果'}</span>
                  <CheckedTime value={report.checkedAt} />
                  {running ? <span>检查完成后更新</span> : null}
                </summary>
                <div className="diagnostic-result-body">
                  <DiagnosticResult report={report} />
                </div>
              </details>
            ) : null}
          </section>
        );
      })}
    </section>
  );
}

export function DiagnosticHistory({ jobs }: { jobs: Job[] }) {
  const ordered = [...jobs].sort((a, b) => b.createdAt - a.createdAt);
  return (
    <details className="panel diagnostic-history" open>
      <summary className="environment-tool-heading">
        <History size={18} aria-hidden="true" />
        <h2>检查记录</h2>
        <span className="diagnostic-history-count">{jobs.length}</span>
        <ChevronDown size={16} className="diagnostic-history-chevron" aria-hidden="true" />
      </summary>
      {ordered.length ? (
        <>
          <p className="diagnostic-history-caption">当前服务运行期间，最近的检查在前。</p>
          <div className="job-list">
            {ordered.map((job) => (
              <div className="job-row" key={job.id}>
                <span className={`job-state-mark ${job.state}`} aria-hidden="true">
                  {job.state === 'completed' ? (
                    <Check size={14} />
                  ) : job.state === 'failed' ? (
                    <X size={14} />
                  ) : (
                    <LoaderCircle size={14} className="spin" />
                  )}
                </span>
                <div className="job-info">
                  <strong>{jobNames[job.kind] ?? job.kind}</strong>
                  <small>
                    <CheckedTime value={job.createdAt} />
                  </small>
                </div>
                <span
                  className={`badge ${job.state === 'completed' ? 'success' : job.state === 'failed' ? 'warning' : 'blue'}`}
                >
                  {job.state === 'completed'
                    ? '已完成'
                    : job.state === 'failed'
                      ? '未通过'
                      : job.state === 'queued'
                        ? '排队中'
                        : '运行中'}
                </span>
                {job.error ? <p>{job.error.message}</p> : null}
              </div>
            ))}
          </div>
        </>
      ) : (
        <div className="diagnostic-history-empty">
          <History size={24} aria-hidden="true" />
          <strong>还没有检查记录</strong>
          <p>运行左侧任一检查后，记录会显示在这里。保存的结果可在对应项目中查看。</p>
        </div>
      )}
    </details>
  );
}

const checkNames: Record<string, string> = {
  native_conversation_metadata_preserved: '原生会话标识透传',
  native_client_headers_preserved: '原生客户端标识透传',
  stream_text: '实时文本、原生标识与用量',
  stream_cancel: '主动取消',
  stream_disconnect: '下游断开',
  stream_timeout: '执行超时',
  stream_truncated: '拒绝截断响应',
  stream_unknown: '拒绝未知事件',
  stream_tools: '工具执行前阻断',
  isolated_no_account: '无个人账户介入',
  reject_wrong_token: '拒绝错误连接令牌',
  two_clients_share_thread: '双连接共享原生会话',
  thread_permission_acknowledgment: '原生权限确认',
  unguarded_control_reproduces_tools: '未过滤对照',
  guard_tools: '工具响应在执行前阻断',
  guard_text: '实时文本与事件关联',
  guard_interrupt: '原生取消与上游关闭',
  guard_unknown: '拒绝未知事件',
  guard_truncated: '拒绝截断响应',
  native_auth_and_no_tools: '认证保留与工具声明移除',
  ordinary_provider_unchanged: '普通会话路由保持',
  offline_no_fallback: '离线无绕行',
};
export const jobNames: Record<string, string> = {
  shared: '桌面连接协议验证',
  stream: '流式输出映射验证',
  guard: '过滤与取消验证',
  provider: '提供方隔离验证',
  'inspect-provider': '读取有效提供方',
  doctor: '桌面状态检查',
  prepare: '准备原生安装',
};
