import { accountGroupIds, availableAccountGroup, sameGroups } from './accountMembership';
import { GroupChoices } from './AccountGroups';
import { useToast } from './Toast';
import { useEffect, useState } from 'react';
import { Settings2 } from 'lucide-react';
import { api } from './api';
import { AccountLogin } from './AccountLogin';
import type { Account, AccountProxy, Operations, Overview } from './api';
import { AccountSummary, accountProxyLabel } from './Accounts';
import { AccountTestChatButton } from './AccountTestChat';
import { NativeAccount } from './NativeAccount';
import { OAuthAccount } from './OAuthAccount';
import { AccountAccess } from './AccountAccess';
import { AccountActivationHelp } from './AccountActivationSwitch';
import { CapabilityStatus } from './RuntimeDetails';
import { AccountScheduling } from './AccountScheduling';
import { AccountRuntimeControls } from './AccountRuntimeControls';
import { accountListUrl } from './navigation';
import { Tabs } from './ui';
import { ProxySettings } from './ProxySettings';
import { LocationSettings, defaultLocation } from './LocationSettings';
import { ApiKeyAccount } from './ApiKeyAccount';
import { AccountClientInfo } from './AccountClientInfo';
import { accountStatus } from './accountStatus';

export function AccountDetails({
  account,
  data,
  token,
  section,
  reload,
}: {
  account: Account;
  data: Overview;
  token: string;
  section: string;
  reload: () => void;
}) {
  const policy = data.operations.accounts.find((item) => item.id === account.id);
  const group = availableAccountGroup(policy, data.operations.groups);
  const status = accountStatus(account, policy, group);
  const [loginBusy, setLoginBusy] = useState(false);
  const [pending, setPending] = useState('');
  const notify = useToast();

  const [groupIds, setGroupIds] = useState(() => accountGroupIds(policy));
  const [baseRevision, setBaseRevision] = useState(data.operations.revision);
  const [formVersion, setFormVersion] = useState(0);
  const [schedulingDirty, setSchedulingDirty] = useState(false);
  useEffect(() => {
    if (!schedulingDirty && sameGroups(groupIds, accountGroupIds(policy)))
      setBaseRevision(data.operations.revision);
  }, [data.operations.revision, schedulingDirty, groupIds, policy]);
  const native = account.source === 'native-config';
  const direct = account.source === 'api-key';
  const currentSection =
    section === 'client' || section === 'runtime'
      ? 'runtime'
      : section === 'access' || section === 'advanced'
        ? section
        : 'login';
  const disabled = !!pending || data.operations.draining || loginBusy;
  const href = (next: string) => {
    const params = new URLSearchParams(location.search);
    params.set('section', next);
    return `?${params}`;
  };
  const saveProxy = async (config: AccountProxy, revision: number) => {
    try {
      const result = await api<Operations>(token, `/accounts/${account.id}/proxy`, {
        revision,
        config,
      });
      notify(
        direct
          ? '账户代理已保存，新请求使用更新后的连接。'
          : '账户代理已保存，正在立即重启运行中的实例。',
      );
      reload();
      return {
        config: result.accounts.find((item) => item.id === account.id)?.proxy ?? config,
        revision: result.revision,
      };
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '保存代理失败', 'error');
      reload();
      return null;
    }
  };

  const resetScheduling = () => {
    setGroupIds(accountGroupIds(policy));
    setBaseRevision(data.operations.revision);
    setFormVersion((value) => value + 1);
    notify('已还原调度表单。');
  };

  const saveScheduling = async (values: { maxConcurrency: number }) => {
    if (!policy) return false;
    setPending('policy');
    try {
      const result = await api<Operations>(token, `/accounts/${account.id}/policy`, {
        revision: baseRevision,
        groupIds,
        enabled: policy.enabled,
        ...values,
      });
      setBaseRevision(result.revision);
      notify('账户调度已保存。');
      reload();
      return true;
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : '保存失败', 'error');
      return false;
    } finally {
      setPending('');
    }
  };

  return (
    <div className="account-detail-layout">
      <div className="detail-heading">
        <a className="text-button" href={accountListUrl(account.id)}>
          返回账户列表
        </a>
        <div className="detail-title-row">
          <h2>{account.name}</h2>
          <span
            className={`badge ${status.tone}`}
            aria-label={`账户状态：${status.label}`}
            title={status.detail}
          >
            <span className="status-dot" aria-hidden="true" />
            {status.label}
          </span>
        </div>
        <p className="helper">
          {account.account?.email ??
            (direct ? 'API Key · 直接连接上游' : native ? '本机 Codex 配置' : '独立账户')}{' '}
          ·{' '}
          {data.operations.groups
            .filter((item) => accountGroupIds(policy).includes(item.id))
            .map((item) => item.name)
            .join('、') || '未分组'}
        </p>
      </div>
      <Tabs
        label="账户配置分类"
        items={[
          ['login', direct ? '上游配置' : '登录与凭据'],
          ['access', '接入与调度'],
          ['advanced', '高级能力'],
          ['runtime', '运行信息'],
        ].map(([key, label]) => ({ label, href: href(key), active: currentSection === key }))}
      />
      {currentSection === 'runtime' ? (
        <div className={`account-runtime-layout${native ? ' account-runtime-layout-native' : ''}`}>
          <section
            className={`panel identity-summary${native ? ' identity-summary-native' : ''}`}
            aria-label="账户运行状态"
          >
            <AccountSummary account={account} policy={policy} group={group} />
            {policy && account.source !== 'shared-desktop' ? (
              <AccountTestChatButton
                account={account}
                policy={policy}
                proxy={accountProxyLabel(policy, data.automaticProxy)}
                token={token}
                disabled={
                  disabled ||
                  !!policy.deleting ||
                  data.operations.draining ||
                  !data.operations.storageHealthy
                }
              />
            ) : null}
          </section>
          <AccountClientInfo account={account} />
        </div>
      ) : null}

      {account.source === 'shared-desktop' ? (
        <section className="panel compact-empty" hidden={currentSection === 'runtime'}>
          <p>此连接用于桌面调试。</p>
          <a className="button secondary" href="?view=settings&tab=diagnostics">
            管理桌面调试
          </a>
        </section>
      ) : (
        <>
          <div hidden={currentSection !== 'login'}>
            {!direct &&
            (native || (!account.credentialSource && !account.account && !policy?.route)) ? (
              <section className="panel detail-form">
                <NativeAccount
                  token={token}
                  account={account}
                  accounts={data.accounts}
                  operations={data.operations}
                  disabled={disabled}
                  onChange={reload}
                />
              </section>
            ) : null}
          </div>
          {direct ? (
            <section className="panel detail-form" hidden={currentSection !== 'login'}>
              <ApiKeyAccount
                key={account.id}
                account={account}
                token={token}
                revision={data.operations.revision}
                disabled={
                  disabled ||
                  !data.operations.storageHealthy ||
                  !!policy?.active ||
                  !!policy?.queued
                }
                onChange={reload}
              />
            </section>
          ) : null}
          {!native && !direct ? (
            <div className="panel detail-form" hidden={currentSection !== 'login'}>
              <OAuthAccount
                id={account.id}
                token={token}
                policy={policy}
                credentialSource={account.credentialSource}
                disabled={disabled}
                onChange={reload}
                loginActions={null}
                loginResult={
                  <AccountLogin
                    id={account.id}
                    token={token}
                    disabled={!!pending || data.operations.draining}
                    onChange={reload}
                    onActiveChange={setLoginBusy}
                  />
                }
              />
            </div>
          ) : null}
          <div className="account-detail-section" hidden={currentSection !== 'access'}>
            {policy ? (
              <div className="account-environment-settings">
                <section className="panel detail-form" id="account-proxy">
                  <ProxySettings
                    key={account.id}
                    value={policy.proxy ?? { mode: 'auto', url: '' }}
                    status={policy.proxyStatus}
                    revision={data.operations.revision}
                    disabled={disabled || !data.operations.storageHealthy || !!policy.deleting}
                    save={saveProxy}
                  />
                </section>
                {!direct ? (
                  <LocationSettings
                    key={account.id}
                    id={account.id}
                    token={token}
                    value={policy.location ?? defaultLocation}
                    status={policy.locationStatus}
                    restart={policy.proxyStatus}
                    revision={data.operations.revision}
                    disabled={disabled || !data.operations.storageHealthy || !!policy.deleting}
                    reload={reload}
                  />
                ) : null}
              </div>
            ) : null}
            <div className="account-control-settings">
              <section className="panel detail-form">
                <AccountAccess
                  apiKey={direct}
                  id={account.id}
                  token={token}
                  policy={policy}
                  groupEnabled={group?.enabled ?? true}
                  keyGroupId={group?.id}
                  revision={data.operations.revision}
                  disabled={disabled || !data.operations.storageHealthy}
                  canEnable={
                    direct
                      ? !!account.upstream?.hasApiKey
                      : !!policy?.route || native || !!account.credentialSource || !!account.account
                  }
                  activationHelp={
                    direct ? undefined : (
                      <AccountActivationHelp
                        account={account}
                        configured={!!policy?.route}
                        installationReady={!!data.installation}
                      />
                    )
                  }
                  onChange={reload}
                />
              </section>
              {policy ? (
                <section className="panel detail-form account-scheduling-card">
                  <div className="section-heading">
                    <h2>
                      <Settings2 size={17} aria-hidden="true" />
                      账户调度
                    </h2>
                    <p>分组与并发上限</p>
                  </div>
                  <div className="settings-scheduling-body">
                    <div className="settings-group-field">
                      <GroupChoices
                        groups={data.operations.groups}
                        value={groupIds}
                        onChange={setGroupIds}
                        disabled={
                          disabled ||
                          pending === 'policy' ||
                          !!policy.active ||
                          !!policy.queued ||
                          !data.operations.storageHealthy ||
                          !!policy.deleting
                        }
                      />
                    </div>
                    <AccountScheduling
                      onDirtyChange={setSchedulingDirty}
                      key={formVersion}
                      account={policy}
                      additionalDirty={!sameGroups(groupIds, accountGroupIds(policy))}
                      cancelLabel="还原调度"
                      disabled={disabled || !data.operations.storageHealthy}
                      pending={pending === 'policy'}
                      cancel={resetScheduling}
                      save={saveScheduling}
                    />
                  </div>
                </section>
              ) : null}
            </div>
          </div>
          {currentSection === 'advanced' ? (
            <section className="panel detail-form">
              <div className="section-heading">
                <div>
                  <h2>运行状态与扩展能力</h2>
                  <p>查看自动接入状态、调整能力开关，或按需运行诊断。</p>
                </div>
              </div>
              {direct ? (
                <p className="helper">
                  请求参数与能力由上游处理。连接与用量可在接入测试和请求记录中查看。
                </p>
              ) : policy?.route ? (
                <CapabilityStatus
                  policy={policy}
                  token={token}
                  revision={data.operations.revision}
                  disabled={disabled}
                  onChange={reload}
                />
              ) : (
                <p className="compact-empty">先在“接入与调度”中启用账户，再查看能力。</p>
              )}
              <AccountRuntimeControls
                account={account}
                policy={policy}
                token={token}
                disabled={disabled}
                canStart={!!data.installation && data.operations.storageHealthy}
                onChange={reload}
              />
            </section>
          ) : null}
        </>
      )}
    </div>
  );
}
