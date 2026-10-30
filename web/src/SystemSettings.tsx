import { useToast } from './Toast';
import { useState } from 'react';
import { InstallationSettings } from './InstallationSettings';
import { api } from './api';
import type { AccessConfig, Overview } from './api';
import { AccessSettingsPage } from './AccessSettings';
import { ConnectionSettings } from './ConnectionSettings';
import { RequestDebug } from './RequestDebug';
import { RequestRetention } from './RequestRetention';
import { Diagnostics, DiagnosticHistory } from './Diagnostics';
import { DesktopDebug } from './DesktopDebug';
import { Tabs } from './ui';
import './connection-settings.css';
import './diagnostics-settings.css';

export function SystemSettings({
  token,
  data,
  tab,
  reload,
  startJob,
  onAccessSaved,
}: {
  token: string;
  data: Overview;
  tab: string;
  reload: () => void;
  startJob: (kind: string) => void;
  onAccessSaved: (config: AccessConfig) => void;
}) {
  const [pending, setPending] = useState(false);
  const notify = useToast();
  const currentTab = ['security', 'diagnostics'].includes(tab) ? tab : 'connection';
  const save = async (path: string, body: object, message: string) => {
    if (pending) return false;
    setPending(true);
    try {
      await api(token, path, { revision: data.operations.revision, ...body });
      notify(message);
      reload();
      return true;
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '保存失败，请重试。', 'error');
      return false;
    } finally {
      setPending(false);
    }
  };
  const preparation = data.jobs.filter((job) => job.kind === 'prepare').at(-1);
  return (
    <>
      <div
        className={
          currentTab === 'connection'
            ? 'connection-settings-width'
            : currentTab === 'diagnostics'
              ? 'environment-width'
              : undefined
        }
      >
        <Tabs
          label="系统设置分类"
          items={[
            ['connection', '连接与调度'],
            ['security', '访问安全'],
            ['diagnostics', '环境与诊断'],
          ].map(([key, label]) => ({
            label,
            href: `?view=settings&tab=${key}`,
            active: currentTab === key,
          }))}
        />
      </div>

      {currentTab === 'connection' ? (
        <div className="connection-settings-width connection-settings-content">
          <ConnectionSettings
            operations={data.operations}
            disabled={pending || data.operations.draining || !data.operations.storageHealthy}
            save={save}
          >
            <RequestRetention
              value={data.operations.retentionDays}
              disabled={pending || data.operations.draining || !data.operations.storageHealthy}
              save={save}
            />
          </ConnectionSettings>
        </div>
      ) : null}
      {currentTab === 'security' ? (
        <AccessSettingsPage token={token} onSaved={onAccessSaved} />
      ) : null}
      {currentTab === 'diagnostics' ? (
        <div className="environment-page environment-width">
          <header className="environment-heading">
            <div>
              <h2>环境与诊断</h2>
              <p>配置原生运行环境，按需检查本机连接与协议。</p>
            </div>
            <a className="text-button" href="?view=accounts">
              管理账户与接入
            </a>
          </header>
          <div className="environment-layout">
            <div className="environment-main">
              <InstallationSettings
                token={token}
                data={data}
                disabled={pending || data.operations.draining || !data.operations.storageHealthy}
                reload={reload}
              />
              <Diagnostics data={data} disabled={pending || data.busy} startJob={startJob} />
            </div>
            <aside className="environment-tools" aria-label="调试工具与检查记录">
              <RequestDebug token={token} />
              <DesktopDebug
                standalone={!!data.installation && !data.installation.desktopExecutable}
                desktop={data.desktop}
                ready={!!data.installation}
                preparation={preparation}
                busy={pending || data.busy}
                prepare={() => startJob('prepare')}
                start={() => void save('/desktop/start', {}, '正在启动桌面调试连接。')}
                stop={() => void save('/desktop/stop', {}, '桌面调试连接已停止。')}
              />
              <DiagnosticHistory jobs={data.jobs} />
            </aside>
          </div>
        </div>
      ) : null}
    </>
  );
}
