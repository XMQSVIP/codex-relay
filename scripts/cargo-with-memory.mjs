/** Run Cargo with a memory preflight; never change build profiles or artifacts. */
import { execFileSync, spawn } from 'node:child_process';
import { availableParallelism, freemem, platform } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const GIB = 1024 ** 3;
export const DEFAULT_MIN_FREE_GIB = 3;
const compiling = new Set(['build', 'b', 'check', 'c', 'clippy', 'test', 't', 'run', 'r', 'rustc', 'doc', 'd', 'rustdoc', 'bench', 'install']);

export function cargoCommand(args) {
  for (let index = 0; index < args.length && args[index] !== '--'; index++) {
    const arg = args[index];
    if (['--config', '--color', '-C', '-Z'].includes(arg)) index++;
    else if (!arg.startsWith('-') && !arg.startsWith('+')) return arg;
  }
  return undefined;
}

export function readAvailableMemory({
  system = platform(), free = freemem, available = () => process.availableMemory(), execute = execFileSync,
} = {}) {
  try {
    const values = [free(), available()];
    if (system === 'win32') {
      // Include virtual-memory headroom: free RAM alone can miss Windows allocation failures.
      const output = execute('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
        '$buildMemoryInfo = Get-CimInstance Win32_OperatingSystem -ErrorAction Stop; @{physical=[double]$buildMemoryInfo.FreePhysicalMemory*1024;virtual=[double]$buildMemoryInfo.FreeVirtualMemory*1024} | ConvertTo-Json -Compress'],
      { encoding: 'utf8', windowsHide: true, timeout: 10000, maxBuffer: 64 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
      const memory = JSON.parse(output.trim());
      values.push(memory.physical, memory.virtual);
    }
    if (values.some(value => !Number.isFinite(value) || value <= 0)) return null;
    return Math.min(...values);
  } catch {
    return null;
  }
}

export function cargoPlan(args, availableBytes, minimumGiB = DEFAULT_MIN_FREE_GIB, {
  cpuCount = availableParallelism(), envJobs = process.env.CARGO_BUILD_JOBS,
} = {}) {
  if (!Number.isFinite(minimumGiB) || minimumGiB <= 0) throw new Error('BRIDGE_BUILD_MIN_FREE_GIB 必须是大于 0 的数字。');
  if (!Number.isSafeInteger(cpuCount) || cpuCount < 1) throw new Error('可用 CPU 数必须是正整数。');
  const unknown = !Number.isFinite(availableBytes) || availableBytes <= 0;
  const ratio = unknown ? 0 : availableBytes / GIB / minimumGiB;
  const memory = unknown ? '无法读取可用内存' : `可用内存 ${(availableBytes / GIB).toFixed(2)} GiB`;
  if (ratio > 2) {
    return { args: [...args], single: false, jobs: null, memoryLimit: null,
      message: `${memory}，高于 ${minimumGiB * 2} GiB，保留原有构建并发设置。` };
  }
  const memoryLimit = ratio < 1 ? 1 : 2;

  // CLI jobs take precedence over the environment; clamp instead of raising a smaller request.
  // Arguments following -- belong to the application/test binary and stay untouched.
  const boundary = args.indexOf('--');
  const cargoArgs = boundary < 0 ? args : args.slice(0, boundary);
  const forwarded = boundary < 0 ? [] : args.slice(boundary);
  const result = [];
  let requestedJobs = envJobs;
  for (let index = 0; index < cargoArgs.length; index++) {
    const arg = cargoArgs[index];
    if (['--config', '--color', '-C', '-Z'].includes(arg)) {
      result.push(arg);
      if (index + 1 < cargoArgs.length) result.push(cargoArgs[++index]);
    } else if (arg === '-j' || arg === '--jobs') {
      if (!cargoArgs[index + 1] || cargoArgs[index + 1].startsWith('--')) throw new Error(`${arg} 缺少并发数量。`);
      requestedJobs = cargoArgs[++index];
    } else if (arg.startsWith('--jobs=')) {
      requestedJobs = arg.slice('--jobs='.length);
    } else if (arg.startsWith('-j') && arg.length > 2) {
      requestedJobs = arg.slice(2).replace(/^=/, '');
    } else {
      result.push(arg);
    }
  }
  let requested = cpuCount;
  if (requestedJobs !== undefined && requestedJobs !== 'default') {
    if (!/^-?\d+$/.test(String(requestedJobs))) throw new Error('Cargo 并发数量必须是非零整数或 default。');
    requested = Number(requestedJobs);
    if (!Number.isSafeInteger(requested) || requested === 0) throw new Error('Cargo 并发数量必须是非零整数或 default。');
    if (requested < 0) requested = Math.max(1, cpuCount + requested);
  }
  const jobs = Math.min(memoryLimit, cpuCount, requested);
  const message = `${memory}，内存档位上限 ${memoryLimit}，使用 ${jobs} 个构建任务（--jobs ${jobs}）。`;
  return { args: [...result, '--jobs', String(jobs), ...forwarded], single: jobs === 1, jobs, memoryLimit, message };
}

async function main() {
  let args = process.argv.slice(2);
  const checkOnly = args.length === 1 && args[0] === '--memory-check';
  const command = cargoCommand(args);
  if (checkOnly || compiling.has(command)) {
    const plan = cargoPlan(args, readAvailableMemory(), Number(process.env.BRIDGE_BUILD_MIN_FREE_GIB ?? DEFAULT_MIN_FREE_GIB));
    console.error(`[构建内存检查] ${plan.message}`);
    if (checkOnly) return;
    args = plan.args;
  }
  const child = spawn('cargo', args, { stdio: 'inherit', windowsHide: true });
  const interrupt = () => { if (process.platform !== 'win32') child.kill('SIGINT'); };
  const terminate = () => child.kill('SIGTERM');
  process.on('SIGINT', interrupt);
  process.on('SIGTERM', terminate);
  await new Promise(resolveExit => {
    child.once('error', error => {
      console.error(`无法启动 Cargo：${error.message}`);
      process.exitCode = 1;
      resolveExit();
    });
    child.once('exit', (code, signal) => {
      process.exitCode = code ?? (signal === 'SIGINT' ? 130 : 1);
      resolveExit();
    });
  });
  process.removeListener('SIGINT', interrupt);
  process.removeListener('SIGTERM', terminate);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
