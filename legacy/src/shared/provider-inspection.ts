import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { readLanProvider } from './provider.js';
import { startBackend } from './backend.js';
import { unusedLoopbackPort } from './probe.js';

/** No thread, inference, credential export, or global config write. */
export async function inspectLanProvider(executable: string) {
  const cwd = path.resolve('.cache/service-workspace');
  await mkdir(cwd, { recursive: true });
  const backend = await startBackend({ executable, port: await unusedLoopbackPort(), cwd });
  try {
    return { purpose: 'effective-lan-provider-inspection', checkedAt: new Date().toISOString(),
      provider: await readLanProvider(backend.rpc, cwd), inferenceDispatched: false,
      note: 'Non-secret metadata only. Credentials remain with the native provider. Real shared-desktop validation is still required.' };
  } finally { await backend.stop(); }
}
