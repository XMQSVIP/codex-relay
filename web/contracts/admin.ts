import { z } from 'zod';

export const debugSettingsSchema = z.object({
  enabled: z.boolean(),
  revision: z.number().int().nonnegative(),
});
export type DebugSettings = z.infer<typeof debugSettingsSchema>;
const contentBodySchema = z.object({
  body: z.unknown(),
  truncated: z.boolean(),
  debug: z.boolean().optional(),
  debugRevision: z.number().int().nonnegative().optional(),
  encoding: z.enum(['utf8', 'base64']).optional(),
  complete: z.boolean().optional(),
});
export const requestContentSchema = z.object({
  id: z.string(),
  request: contentBodySchema.extend({ textOmitted: z.boolean() }).nullable(),
  response: contentBodySchema
    .extend({ transport: z.string(), statusCode: z.number().nullable() })
    .nullable(),
});
export type RequestContent = z.infer<typeof requestContentSchema>;

export const nativeAffinitySettingsSchema = z.object({
  nativeAffinityTtlHours: z.number().int().min(1).max(720),
});
export type NativeAffinitySettings = z.infer<typeof nativeAffinitySettingsSchema>;

export const nativeCapabilitiesSchema = z.object({
  installation: z.string(),
  provider: z.string(),
  checkedAt: z.number(),
  stale: z.boolean(),
  models: z.array(
    z.object({
      id: z.string(),
      model: z.string(),
      inputModalities: z.array(z.string()).nullable(),
      supportedReasoningEfforts: z
        .array(z.object({ reasoningEffort: z.string(), description: z.string() }))
        .nullable(),
      serviceTiers: z
        .array(z.object({ id: z.string(), name: z.string(), description: z.string() }))
        .nullable(),
      defaultServiceTier: z.string().nullable(),
    }),
  ),
  webSearch: z.boolean().nullable(),
  imageGeneration: z.boolean().nullable(),
  errorCode: z.string().nullable(),
  verifiedFeatures: z
    .array(z.enum(['imageInput', 'namespacedTools', 'webSearch', 'imageGeneration']))
    .optional(),
  verifiedModel: z.string().nullable().optional(),
  verificationErrors: z.record(z.string(), z.string()).optional(),
});
export type NativeCapabilities = z.infer<typeof nativeCapabilitiesSchema>;

export const accountMembershipSchema = z.object({
  groupIds: z.array(z.string().uuid()).min(1).max(64),
  groupId: z.never().optional(),
});
export type AccountMembership = Pick<z.infer<typeof accountMembershipSchema>, 'groupIds'>;

// This is the source of the console types for these responses. Only tests load
// the validators; production code imports their inferred types through api.ts.
export const accountSubscriptionSchema = z.object({
  status: z.enum(['unknown', 'unsupported', 'current', 'stale', 'error']),
  planType: z.string().nullable().optional(),
  startsAt: z.number().nullable().optional(),
  expiresAt: z.number().nullable().optional(),
  willRenew: z.boolean().nullable().optional(),
  checkedAt: z.number().optional(),
  lastAttemptAt: z.number().optional(),
  nextAttemptAt: z.number().optional(),
  errorCode: z.string().nullable().optional(),
});

export const accountSchema = z.object({
  id: z.string(),
  name: z.string(),
  createdAt: z.number(),
  state: z.string(),
  pid: z.number().nullable(),
  account: z
    .object({
      type: z.string(),
      email: z.string().nullable(),
      planType: z.string().nullable(),
    })
    .nullable(),
  subscription: accountSubscriptionSchema.nullable().optional(),
  source: z.enum(['isolated', 'native-config', 'shared-desktop', 'api-key']).optional(),
  upstream: z
    .object({
      baseUrl: z.string(),
      transport: z.enum(['http-sse', 'ws-first']),
      hasApiKey: z.boolean(),
    })
    .nullable()
    .optional(),
  credentialSource: z
    .enum(['managed', 'external', 'desktop', 'native-login', 'api-key'])
    .nullable()
    .optional(),
  planType: z.string().nullable().optional(),
  clients: z
    .array(
      z.object({
        pid: z.number(),
        clientInfo: z.object({ name: z.string(), title: z.string(), version: z.string() }),
        userAgent: z.string().nullable(),
      }),
    )
    .optional(),
});

export const accountQuotaSchema = z.object({
  status: z.enum(['unknown', 'supported', 'unsupported', 'error']),
  checkedAt: z.number().nullable().optional(),
  lastAttemptAt: z.number().optional(),
  stale: z.boolean(),
  blocked: z.boolean(),
  retryAt: z.number().nullable().optional(),
  errorCode: z.string().nullable().optional(),
  resetContext: z.string().nullable().optional(),
  credits: z
    .object({
      balance: z.string().nullable(),
      hasCredits: z.boolean().nullable(),
      unlimited: z.boolean(),
      checkedAt: z.number(),
    })
    .nullable()
    .optional(),
  resetCredits: z
    .object({
      availableCount: z.number(),
      checkedAt: z.number(),
      credits: z
        .array(
          z.object({
            id: z.string(),
            resetType: z.string().nullable(),
            status: z.string().nullable(),
            expiresAt: z.number().nullable(),
          }),
        )
        .nullable(),
    })
    .nullable()
    .optional(),
  windows: z.array(
    z.object({
      bucket: z.string(),
      window: z.string(),
      usedPercent: z.number().nullable(),
      durationMins: z.number().nullable(),
      resetsAt: z.number().nullable(),
      reached: z.boolean(),
      usage: z
        .object({
          from: z.number(),
          to: z.number(),
          requestCount: z.number(),
          usageKnownRequests: z.number(),
          totalTokens: z.number().nullable(),
        })
        .nullable()
        .optional(),
    }),
  ),
});

export const schedulingDecisionSchema = z.object({
  version: z.string(),
  mode: z.string(),
  candidateCount: z.number(),
  score: z.number().nullable(),
  components: z
    .object({
      load: z.number(),
      quota: z.number(),
      health: z.number(),
      latency: z.number(),
    })
    .nullable(),
  affinity: z.string(),
});

export const requestRecordSchema = z.object({
  endpoint: z.string().nullable(),
  clientTransport: z.enum(['ws', 'sse', 'http']).nullable(),
  upstreamTransport: z.enum(['ws', 'sse', 'http', 'native']).nullable(),
  id: z.string(),
  startedAt: z.number(),
  kind: z.string(),
  action: z.string(),
  accountId: z.string().nullable(),
  accountName: z.string().nullable(),
  groupId: z.string().nullable(),
  groupName: z.string().nullable(),
  status: z.string(),
  durationMs: z.number().nullable(),
  waitMs: z.number().nullable(),
  errorCode: z.string().nullable(),
  serviceTier: z.string().nullable().optional(),
  responseServiceTier: z.string().nullable(),
  subagentKind: z.string().nullable().optional(),
  scheduling: schedulingDecisionSchema.nullable().optional(),
  parameterHandling: z
    .array(z.object({ path: z.string(), action: z.string(), reason: z.string() }))
    .optional(),
  streaming: z.boolean().nullable().optional(),
  reasoningEffort: z.string().nullable().optional(),
  userAgent: z.string().nullable().optional(),
  keyName: z.string().nullable().optional(),
  totalMs: z.number().nullable().optional(),
  firstResponseMs: z.number().nullable().optional(),
  keyId: z.string().nullable().optional(),
  model: z.string().nullable().optional(),
  responseModel: z.string().nullable().optional(),
  inputTokens: z.number().nullable().optional(),
  outputTokens: z.number().nullable().optional(),
  cachedInputTokens: z.number().nullable().optional(),
  nativeThreadId: z.string().nullable().optional(),
  nativeTurnId: z.string().nullable().optional(),
  modelOutcome: z.string().nullable().optional(),
  failureStage: z.string().nullable().optional(),
  cleanupErrorCode: z.string().nullable().optional(),
  firstDeltaMs: z.number().nullable().optional(),
  deliveryStatus: z.string().nullable().optional(),
});

export const usageRankingSchema = z.object({
  id: z.string(),
  name: z.string().nullable(),
  requestCount: z.number(),
  failedRequests: z.number(),
  averageFirstResponseMs: z.number().nullable(),
  averageModelFirstResponseMs: z.number().nullable(),
  averageTotalMs: z.number().nullable(),
  cachedInputTokens: z.number().nullable(),
  cacheKnownInputTokens: z.number().nullable(),
  cacheKnownRequests: z.number(),
  cacheHitRate: z.number().nullable(),
  inputTokens: z.number().nullable(),
  outputTokens: z.number().nullable(),
  totalTokens: z.number().nullable(),
  usageKnownRequests: z.number(),
});

export const statisticsSchema = z.object({
  from: z.number(),
  to: z.number(),
  hours: z.number(),
  page: z.number(),
  pageSize: z.number(),
  summary: z.object({
    total: z.number(),
    succeeded: z.number(),
    failed: z.number(),
    rejected: z.number(),
    cancelled: z.number(),
    running: z.number(),
    queued: z.number(),
    timedOut: z.number(),
    queueFull: z.number(),
    averageWaitMs: z.number().nullable(),
    averageMs: z.number().nullable(),
    averageFirstResponseMs: z.number().nullable(),
    averageModelFirstResponseMs: z.number().nullable(),
    averageTotalMs: z.number().nullable(),
    inferenceRequests: z.number(),
    inferenceSucceeded: z.number(),
    inferenceFailed: z.number(),
    totalTokens: z.number().nullable(),
    inputTokens: z.number().nullable(),
    outputTokens: z.number().nullable(),
    usageKnownRequests: z.number(),
    cachedInputTokens: z.number().nullable(),
    cacheKnownInputTokens: z.number().nullable(),
    cacheKnownRequests: z.number(),
    cacheHitRate: z.number().nullable(),
  }),
  trend: z.array(
    z.object({
      startedAt: z.number(),
      count: z.number(),
      succeeded: z.number(),
      rejected: z.number(),
      inferenceRequests: z.number(),
      inputTokens: z.number().nullable(),
      outputTokens: z.number().nullable(),
      totalTokens: z.number().nullable(),
      usageKnownRequests: z.number(),
      cachedInputTokens: z.number().nullable(),
      cacheKnownInputTokens: z.number().nullable(),
      cacheKnownRequests: z.number(),
      cacheHitRate: z.number().nullable(),
    }),
  ),
  records: z.array(requestRecordSchema),
  accountUsage: z.array(usageRankingSchema),
  keyUsage: z.array(usageRankingSchema),
  modelUsage: z.array(usageRankingSchema),
});

export type AccountSubscriptionInfo = z.infer<typeof accountSubscriptionSchema>;
export type Account = z.infer<typeof accountSchema>;
export type AccountQuota = z.infer<typeof accountQuotaSchema>;
export type SchedulingDecision = z.infer<typeof schedulingDecisionSchema>;
export type RequestRecord = z.infer<typeof requestRecordSchema>;
export type UsageRanking = z.infer<typeof usageRankingSchema>;
export type Statistics = z.infer<typeof statisticsSchema>;

export const accountBackupSchema = z.object({
  format: z.literal('codex-relay-accounts'),
  version: z.literal(1),
  items: z.array(
    z
      .object({
        name: z.string(),
        source: z.string(),
        groupIds: z.array(z.string()).min(1).max(64),
        groupNames: z.array(z.string()),
        maxConcurrency: z.number(),
        requiresReconnect: z.boolean(),
        document: z.record(z.string(), z.unknown()).nullable(),
        upstream: z
          .object({
            baseUrl: z.string(),
            apiKey: z.string().min(1).max(8192),
            transport: z.enum(['http-sse', 'ws-first']),
          })
          .optional(),
      })
      .refine((item) => item.source !== 'api-key' || item.upstream !== undefined, {
        path: ['upstream'],
        message: 'API Key 备份必须包含上游配置',
      }),
  ),
});
export const accountBatchResultSchema = z.object({
  action: z.enum(['configure', 'delete', 'import']),
  status: z.enum(['completed', 'partial', 'conflict']),
  items: z.array(
    z.object({
      accountId: z.string().optional(),
      status: z.enum(['succeeded', 'failed', 'skipped']),
      code: z.string().optional(),
    }),
  ),
});
export type AccountBackup = z.infer<typeof accountBackupSchema>;
export type AccountBatchResult = z.infer<typeof accountBatchResultSchema>;
export const installationPathsSchema = z.object({
  appServerExecutable: z.string().min(1),
});
export const installationSchema = installationPathsSchema.extend({
  desktopExecutable: z.string().min(1).nullable(),
  archive: z.string().min(1).nullable(),
  manuallyConfigured: z.boolean(),
  appServerVersion: z.string().nullable(),
  appServerSha256: z.string(),
});
export type InstallationPaths = z.infer<typeof installationPathsSchema>;
export type Installation = z.infer<typeof installationSchema>;
