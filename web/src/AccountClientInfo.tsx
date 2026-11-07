import type { Account } from './api';
import './account-client-info.css';

export function AccountClientInfo({ account }: { account: Account }) {
  if (account.source === 'api-key') return null;
  const clients = account.clients ?? [];
  return <section className="panel account-client-info" aria-label="当前 clientInfo">
    <div className="section-heading"><h2>当前 clientInfo</h2><span className="helper">运行实例使用的客户端身份</span></div>
    {clients.length ? clients.map(client => <div className="account-client-instance" key={client.pid}>
      <p className="helper">实例 PID {client.pid}</p>
      <dl><div><dt>名称（name）</dt><dd>{client.clientInfo.name}</dd></div><div><dt>标题（title）</dt><dd>{client.clientInfo.title}</dd></div><div><dt>版本（version）</dt><dd>{client.clientInfo.version}</dd></div></dl>
      {client.userAgent ? <details open><summary>User-Agent</summary><p>{client.userAgent}</p></details> : null}
    </div>) : <p className="helper">当前没有可读取身份的运行实例。实例启动后显示实际使用的 clientInfo。</p>}
  </section>;
}
