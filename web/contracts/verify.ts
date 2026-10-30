import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { z } from 'zod';
import {
  installationSchema,
  debugSettingsSchema,
  nativeCapabilitiesSchema,
  requestContentSchema,
  nativeAffinitySettingsSchema,
  accountMembershipSchema,
  accountSchema,
  accountQuotaSchema,
  statisticsSchema,
  accountBackupSchema,
  accountBatchResultSchema,
} from './admin.ts';

const cases = <T extends z.ZodType>(schema: T) =>
  z.array(z.object({ name: z.string(), value: schema })).nonempty();
const corpusSchema = z.object({
  overviewInstallation: installationSchema.nullable(),
  installations: cases(installationSchema),
  debugSettings: cases(debugSettingsSchema),
  requestContents: cases(requestContentSchema),
  affinitySettings: cases(nativeAffinitySettingsSchema),
  operations: z.object({
    accounts: z
      .array(accountMembershipSchema.extend({ capabilities: nativeCapabilitiesSchema.nullable() }))
      .nonempty(),
  }),
  accountBackup: accountBackupSchema,
  accountImport: accountBatchResultSchema,
  accounts: z.array(accountSchema).min(3),
  quotas: cases(accountQuotaSchema),
  statistics: cases(statisticsSchema),
});

const path = process.argv[2];
assert.ok(path, 'Expected the Rust management-response corpus path');
const result = corpusSchema.safeParse(JSON.parse(readFileSync(path, 'utf8')));
if (!result.success) {
  for (const issue of result.error.issues) {
    console.error(issue.path.join('.') + ': ' + issue.message);
  }
  process.exit(1);
}
const corpus = result.data;
assert.ok(
  corpus.operations.accounts.some((account) =>
    account.capabilities?.models.some((model) =>
      model.serviceTiers?.some((tier) => tier.id === 'ultrafast'),
    ),
  ),
);
assert.ok(
  corpus.operations.accounts.some((account) =>
    account.capabilities?.models.some((model) => model.serviceTiers === null),
  ),
);
assert.ok(corpus.accountBackup.items.some((item) => item.document?.tokens));
const apiBackup = corpus.accountBackup.items.find((item) => item.source === 'api-key');
assert.ok(apiBackup?.upstream);
assert.equal(apiBackup.upstream.baseUrl, 'http://127.0.0.1:9/v1');
assert.equal(apiBackup.upstream.transport, 'http-sse');
assert.equal(apiBackup.document, null);
assert.equal(apiBackup.upstream.apiKey, 'synthetic-contract-only');
assert.equal(apiBackup.requiresReconnect, false);
assert.ok(!JSON.stringify(corpus.accounts).includes('synthetic-contract-only'));
assert.ok(
  corpus.accountBackup.items.some((item) => item.requiresReconnect && item.document === null),
);
assert.equal(corpus.accountImport.items[0].code, 'DUPLICATE_ACCOUNT');
assert.ok(corpus.quotas.some((row) => row.value.checkedAt === null));
assert.ok(corpus.quotas.some((row) => row.value.blocked));
assert.ok(corpus.quotas.some((row) => row.value.resetCredits?.credits?.length));
assert.deepEqual(
  new Set(corpus.quotas.map((row) => row.value.status)),
  new Set(['unknown', 'supported', 'unsupported', 'error']),
);
assert.ok(corpus.statistics.some((row) => row.value.records.length === 0));
assert.deepEqual(
  new Set(
    corpus.statistics.flatMap((row) =>
      row.value.records.map((record) => record.responseServiceTier),
    ),
  ),
  new Set([null, 'default', 'priority']),
);
assert.ok(
  corpus.statistics.some((row) => row.value.records.some((record) => record.inputTokens === null)),
);
assert.ok(
  corpus.statistics.some((row) => row.value.records.some((record) => record.inputTokens === 0)),
);
console.log(
  'PASS: real Rust management responses match console account, quota and statistics contracts',
);
