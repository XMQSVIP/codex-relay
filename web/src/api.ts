import type {
  NativeCapabilities,
  NativeAffinitySettings,
  AccountMembership,
  AccountSubscriptionInfo,
  Account,
  AccountQuota,
  SchedulingDecision,
  RequestRecord,
  UsageRanking,
  Statistics,
  Installation,
} from '../contracts/admin';
export type { AccountBackup, AccountBatchResult } from '../contracts/admin';
export type { Installation, InstallationPaths } from '../contracts/admin';
export type {
  NativeCapabilities,
  AccountSubscriptionInfo,
  Account,
  AccountQuota,
  SchedulingDecision,
  RequestRecord,
  UsageRanking,
  Statistics,
};

export interface Check {
  id: string;
  state: string;
  detail: string;
}
export interface Report {
  purpose?: string;
  checks?: Check[];
  provider?: { id: string; model: string; baseUrl: string; auth: string };
  checkedAt?: number;
  sharedDesktopVerified?: boolean;
  installation?: Installation | null;
  desktops?: { pid: number; name: string; executable: string | null }[];
  native?: { ownerPid: number; verified: boolean } | null;
}
export interface Job {
  id: string;
  kind: string;
  state: string;
  createdAt: number;
  report: Report | null;
  error: { code: string; message: string } | null;
}
export interface Overview {
  automaticProxy: {
    state: 'proxy' | 'direct' | 'error';
    http: string | null;
    https: string | null;
  };
  implementation: string;
  version: string;
  inferenceEnabled: boolean;
  busy: boolean;
  installation: Installation | null;
  desktop: {
    inferenceEnabled?: boolean;
    provider?: { model: string; id: string; baseUrl: string };
    routeError?: { code: string; message: string };
    state: string;
    managed?: boolean;
    error?: { code: string; message: string };
    backendPid?: number;
    desktopPid?: number;
    lanVerification?: string;
  };
  accounts: Account[];
  jobs: Job[];
  reports: Record<string, Report>;
  operations: Operations;
}
export interface Group {
  id: string;
  name: string;
  enabled: boolean;
  active: number;
  queued: number;
  memberCount: number;
}
export type NativeFeature = 'imageInput' | 'namespacedTools' | 'webSearch' | 'imageGeneration';
export interface NativeHealth {
  state: 'available' | 'cooling' | 'halfOpen';
  consecutiveFailures: number;
  retryAt: number | null;
  reason: string | null;
  sharedUpstream: boolean;
  lastVerifiedAt: number | null;
}
export interface AccountLocation {
  enabled: boolean;
  source: 'proxy' | 'custom';
  custom: { timezone: string; country: string; region: string; city: string };
}
export interface LocationStatus {
  state: 'disabled' | 'pending' | 'detecting' | 'applied' | 'cached' | 'failed';
  effective?: AccountLocation['custom'] | null;
  detectedAt?: number | null;
  error?: string | null;
}
export interface AccountPolicy extends AccountMembership {
  location?: AccountLocation;
  locationStatus?: LocationStatus;
  proxy?: AccountProxy;
  proxyStatus?: ProxyStatus;
  quota?: AccountQuota;
  features?: NativeFeature[];
  disabledFeatures?: NativeFeature[];
  capabilities?: NativeCapabilities | null;
  health?: NativeHealth;
  deleting?: boolean;
  id: string;
  name: string;
  enabled: boolean;
  maxConcurrency: number;
  active: number;
  queued: number;
  route?: { id: string; model: string; baseUrl: string; auth?: string };
}
export interface AccountProxy {
  mode: 'auto' | 'direct' | 'custom';
  url: string;
}
export interface ProxyStatus {
  state: 'applied' | 'switching' | 'failed';
  error?: { code: string; message: string } | null;
}
export interface Operations extends NativeAffinitySettings {
  responseBindings?: { persistent: true; bodiesStored: false } & (
    { count: number; bytes: number; maxBytes: number; ttlSeconds: number } | { error: string }
  );
  revision: number;
  globalLimit: number;
  globalLimitAuto: boolean;
  manualGlobalLimit: number;
  accountCapacity: number;
  active: number;
  queued: number;
  queueLimit: number;
  queueTimeoutSeconds: number;
  storageHealthy: boolean;
  draining: boolean;
  retentionDays: number;
  groups: Group[];
  accounts: AccountPolicy[];
}
export interface QueueSnapshot {
  page: number;
  pageSize: number;
  total: number;
  totalQueued: number;
  records: {
    id: string;
    position: number;
    startedAt: number;
    kind: string;
    action: string;
    accountId: string | null;
    accountName: string | null;
    groupId: string | null;
    groupName: string | null;
    waitMs: number;
    remainingMs: number;
  }[];
}
export class ApiError extends Error {
  constructor(
    message: string,
    public readonly code: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}
export interface AccountMetric extends Omit<UsageRanking, 'name' | 'id'> {
  id: string | null;
  succeeded: number;
  failed: number;
  successRate: number | null;
  cachedInputTokens: number | null;
  lastUsedAt?: number | null;
}
export interface AccountMetrics {
  from: number;
  to: number;
  updatedAt: number;
  accounts: AccountMetric[];
  models: AccountMetric[];
}
export type { RequestContent } from '../contracts/admin';

export interface Login {
  type: string;
  loginId: string;
  status: 'waiting' | 'completing' | 'succeeded' | 'failed' | 'cancelled' | 'expired';
  startedAt: number;
  expiresAt: number;
  finishedAt?: number;
  authUrl?: string;
  verificationUrl?: string;
  userCode?: string;
  manualCallbackAvailable: boolean;
  manualCallbackReason?: string;
  error?: { code: string; message: string } | null;
  warning?: string;
}
export interface ClientKey {
  fastMode?: boolean;
  secret?: string | null;
  id: string;
  name: string;
  prefix: string;
  groupId: string;
  models: string[];
  state: 'active' | 'paused' | 'expired' | 'revoked';
  createdAt: number;
  expiresAt: number | null;
  lastAuthenticatedAt: number | null;
  revision: number;
  maxConcurrency: number;
}
export interface NetworkEndpoint {
  listen: string;
  baseUrl: string;
  transport: 'http' | 'https' | 'reverse-proxy';
}
export interface ClientKeys {
  keys: ClientKey[];
  baseUrl: string;
  localBaseUrl: string;
  lanBaseUrls?: string[];
  networkEndpoints: NetworkEndpoint[];
  inferenceEnabled: boolean;
}
export interface CreatedClientKey {
  key: ClientKey;
  secret: string;
}
export interface AccessConfig {
  passwordEnabled: boolean;
  allowPublicAccess: boolean;
}
export interface AdminSession extends AccessConfig {
  requiresToken: boolean;
  authenticated: boolean;
}

export function initialToken(): string {
  const hash = new URLSearchParams(location.hash.slice(1));
  const token = hash.get('token');
  if (token) {
    history.replaceState(null, '', location.pathname + location.search);
    if (/^[a-f0-9]{64}$/.test(token)) {
      try {
        sessionStorage.setItem('bridge-admin', token);
      } catch {
        /* Memory-only session remains usable. */
      }
      return token;
    }
  }
  try {
    return sessionStorage.getItem('bridge-admin') ?? '';
  } catch {
    return '';
  }
}

export async function api<T>(
  token: string,
  path: string,
  body?: unknown,
  signal?: AbortSignal,
  method?: string,
): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method: method ?? (body === undefined ? 'GET' : 'POST'),
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal,
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    if (data?.error?.code === 'ADMIN_LOGIN_REQUIRED')
      window.dispatchEvent(new Event('bridge-auth-required'));
    throw new ApiError(
      data?.error?.message ?? `请求未完成（${response.status}）`,
      data?.error?.code ?? 'HTTP_ERROR',
      response.status,
    );
  }
  return data as T;
}
