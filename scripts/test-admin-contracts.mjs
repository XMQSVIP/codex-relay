import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, rmdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const temporary = mkdtempSync(join(tmpdir(), 'relay-admin-contracts-'));
const output = join(temporary, 'responses.json');
function run(args, env = process.env) {
  const result = spawnSync(process.execPath, args, {
    cwd: root,
    env,
    stdio: 'inherit',
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error('Contract check command failed (' + (result.signal ?? result.status) + ')');
}
try {
  run(['scripts/cargo-with-memory.mjs', 'test', '--locked', '--test', 'admin_contracts'], {
    ...process.env,
    BRIDGE_ADMIN_CONTRACT_OUTPUT: output,
  });
  run(['--import', 'tsx', 'web/contracts/verify.ts', output]);
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
} finally {
  rmSync(output, { force: true });
  rmdirSync(temporary);
}
