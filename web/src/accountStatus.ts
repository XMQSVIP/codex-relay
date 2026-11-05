import type { Account, AccountPolicy, Group } from './api';

export type AccountStatus = { label: string; detail: string; tone: 'neutral' | 'success' | 'warning' | 'blue'; category: 'available' | 'limited' | 'pending' | 'disabled' | 'paused' | 'debug' };
export const hasCredentials = (a: Account) => a.source === 'api-key' ? !!a.upstream?.hasApiKey : a.source === 'native-config' || !!a.credentialSource || !!a.account;
export function accountStatus(account: Account, policy?: AccountPolicy, group?: Group): AccountStatus {
  const result = (label: string, detail: string, category: AccountStatus['category'], tone: AccountStatus['tone'] = 'neutral'): AccountStatus => ({ label, detail, category, tone });
  if (account.source === 'shared-desktop') return result('不承接 API', '桌面连接仅用于调试', 'debug');
  if (!policy) return result('状态待同步', '请刷新账户状态', 'pending');
  if (policy.deleting) return result('正在删除', '等待账户清理完成', 'pending', 'warning');
  if (!policy.enabled) return result(policy.active ? '停用中' : '已停用', policy.active ? '不接收新请求，当前任务完成后停止实例' : '不接收新请求，配置与凭据已保留', 'disabled');
  if (!policy.route) return result('待配置', hasCredentials(account) ? '启用后可接收客户端请求' : '请先登录或导入凭据', 'pending');
  if (!group?.enabled) return result('分组已暂停', '在“分组管理”恢复分组', 'paused');
  if (policy.proxyStatus?.state === 'failed') return result('代理切换失败', '请在账户代理设置中重试，配置已保留', 'limited', 'warning');
  if (policy.proxyStatus?.state === 'switching') return result('代理切换中', '正在重启账户实例，新请求排队等待', 'limited', 'blue');
  if (policy.quota?.blocked) return result('额度受限', '等待额度恢复确认，现有请求继续运行', 'limited', 'warning');
  if (policy.health?.state === 'cooling') return result('冷却中', '上游暂不可用，等待恢复检查', 'limited', 'warning');
  if (policy.health?.state === 'halfOpen') return result('等待恢复', '下次请求将检查恢复状态', 'limited', 'warning');
  if (account.state === 'unavailable' || account.state === 'disconnected') return result('状态异常', '无法读取实例状态，请刷新或检查原生连接', 'pending', 'warning');
  return result('可调度', '调用模型由客户端选择', 'available', 'success');
}
const reasons: Record<string, string> = {
  RPC_CLOSED: '原生连接已断开，可检查原生安装与进程状态。', RPC_TIMEOUT: '原生后端响应超时，等待恢复检查。',
  RPC_PROTOCOL: '原生协议异常，请检查安装版本。', BACKEND_START: '原生后端启动失败，请检查安装。',
  BACKEND_SPAWN: '无法启动原生进程，请检查权限与安装。', IO_ERROR: '本机读写异常，请检查磁盘与权限。',
  STREAM_EVENTS_LOST: '原生事件连接中断，等待恢复检查。', UPSTREAM_RATE_LIMITED: '上游暂时限流，请等待后重试。',
  UPSTREAM_CAPACITY: '上游容量暂时不足，请等待恢复。', OAUTH_TOKEN_EXPIRED: '凭据已过期，请重新登录或导入。',
};
export const safeErrorCode = (code: string | null | undefined) => code && /^[A-Z0-9_]{1,96}$/.test(code) ? code : null;
export const healthReason = (code: string | null | undefined) => reasons[code ?? ''] ?? '请检查账户连接；未知错误可通过请求记录排查。';
