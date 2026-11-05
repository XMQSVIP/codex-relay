import { parseArgs } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { doctor } from './doctor.js';
import { inspectArchive } from './desktop/asar.js';
import { BridgeError, errorCode } from './errors.js';
import { prepareInstallation, readInstallation, validateInstallation } from './shared/installation.js';
import { probeSharedBackend } from './shared/probe.js';
import { runSharedRuntime, readRuntimeStatus } from './shared/runtime.js';
import { probeToolIsolation } from './shared/isolation-probe.js';
import { probeResponseGuard } from './shared/guard-probe.js';
import { inspectLanProvider } from './shared/provider-inspection.js';
import { probeProviderRouting } from './shared/provider-probe.js';

function positiveInteger(value: string | undefined, name: string, max: number): number | undefined {
  if (value === undefined) return undefined;
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) < 1 || Number(value) > max) {
    throw new BridgeError('CLI_ARGUMENT', `${name} must be an integer between 1 and ${max}.`);
  }
  return Number(value);
}

async function main() {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      'desktop-pid': { type: 'string' }, 'backend-port': { type: 'string' }, 'desktop-port': { type: 'string' },
      report: { type: 'string' }, archive: { type: 'string' }, help: { type: 'boolean', short: 'h' },
      'verify-lan': { type: 'boolean' },
    },
  });
  if (values.help) {
    console.log('LAN provider tools: pnpm run inspect:provider [--report .cache/lan-provider.json]\npnpm run probe:provider [--report .cache/provider-probe.json]\npnpm run start:shared --verify-lan (two fixed real diagnostic prompts after the desktop connects)');
    console.log('pnpm run prepare:desktop [--desktop-pid PID]\npnpm run probe:shared [--report .cache/shared-probe.json]\npnpm run probe:tools [--report .cache/isolation-probe.json]\npnpm run probe:guard [--report .cache/guard-probe.json]\npnpm run start:shared [--backend-port 4500] [--desktop-port 4501]\npnpm run status:shared\npnpm run doctor [--desktop-pid PID] [--report .cache/doctor.json]\npnpm run inspect [--archive PATH] [--report .cache/source-inspection.json]\nExit codes: 0 = requested operation succeeded; 1 = error; 2 = requested validation failed or release gate not satisfied. Probe success does not enable inference.');
    return;
  }
  const command = positionals[0];
  if (positionals.length !== 1 || !['doctor', 'inspect', 'inspect-provider', 'prepare', 'probe', 'probe-tools', 'probe-guard', 'probe-provider', 'launch', 'status'].includes(command!)) throw new BridgeError('CLI_ARGUMENT', 'Unknown command; use --help for supported operations.');
  const desktopPid = positiveInteger(values['desktop-pid'], '--desktop-pid', 2 ** 32 - 1);
  const options = { ...(desktopPid === undefined ? {} : { desktopPid }) };
  if (values.archive && command !== 'inspect') throw new BridgeError('CLI_ARGUMENT', '--archive is only supported by inspect.');
  if ((values['backend-port'] || values['desktop-port']) && command !== 'launch') throw new BridgeError('CLI_ARGUMENT', 'Port options are only supported by launch.');
  if (values['verify-lan'] && command !== 'launch') throw new BridgeError('CLI_ARGUMENT', '--verify-lan requires launch and a connected shared desktop.');
  let report: unknown;
  if (command === 'doctor') {
    const result = await doctor(options);
    report = result;
    process.exitCode = result.gate.ready ? 0 : 2;
  } else if (command === 'inspect') {
    const archive = values.archive ?? (await doctor(options)).desktop?.archive;
    if (!archive) throw new BridgeError('NO_ARCHIVE', 'Cannot locate desktop archive. Supply --archive explicitly.');
    report = await inspectArchive(archive);
  } else if (command === 'prepare') {
    report = await prepareInstallation(desktopPid);
  } else if (command === 'inspect-provider') {
    const installation = await readInstallation();
    await validateInstallation(installation);
    report = await inspectLanProvider(installation.appServerExecutable);
  } else if (command === 'probe' || command === 'probe-tools' || command === 'probe-guard' || command === 'probe-provider') {
    const installation = await readInstallation();
    await validateInstallation(installation);
    const result = command === 'probe' ? await probeSharedBackend(installation.appServerExecutable)
      : command === 'probe-tools' ? await probeToolIsolation(installation.appServerExecutable)
      : command === 'probe-provider' ? await probeProviderRouting(installation.appServerExecutable) : await probeResponseGuard(installation.appServerExecutable);
    report = result;
    process.exitCode = result.checks.some(check => check.state === 'blocked') ? 2 : 0;
  } else if (command === 'status') {
    report = await readRuntimeStatus() ?? { state: 'not-started', inferenceEnabled: false };
  } else {
    if (values.report || desktopPid !== undefined) throw new BridgeError('CLI_ARGUMENT', 'launch accepts port options and --verify-lan only.');
    await runSharedRuntime({ backendPort: positiveInteger(values['backend-port'], '--backend-port', 65535) ?? 4500, desktopPort: positiveInteger(values['desktop-port'], '--desktop-port', 65535) ?? 4501,
      ...(values['verify-lan'] ? { verifyLan: true } : {}),
    });
    return;
  }
  const text = JSON.stringify(report, null, 2) + '\n';
  if (values.report) {
    const filename = path.resolve(values.report);
    // Reports must not overwrite source/config files by accident.
    if (!filename.endsWith('.json') || !filename.startsWith(path.resolve('.cache') + path.sep)) {
      throw new BridgeError('REPORT_PATH', 'Reports must be JSON files inside this workspace .cache directory.');
    }
    await mkdir(path.dirname(filename), { recursive: true });
    await writeFile(filename, text, 'utf8');
  }
  console.log(text);
}

main().catch((error: unknown) => {
  console.error(JSON.stringify({ error: errorCode(error), message: error instanceof BridgeError ? error.message : 'Command failed; no configuration was changed.' }));
  process.exitCode = 1;
});
