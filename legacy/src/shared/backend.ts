import { spawn, type ChildProcess } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { AppServerRpc } from '../transport/rpc.js';
import { BridgeError } from '../errors.js';

export function cleanChildEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.toUpperCase().startsWith('CODEX_') || name.toUpperCase() === 'CODEX_HOME'));
}

export interface Backend {
  child: ChildProcess;
  endpoint: string;
  token: string;
  rpc: AppServerRpc;
  exited: Promise<void>;
  stop(): Promise<void>;
}

export async function startBackend(options: { executable: string; port: number; cwd: string; home?: string; isolated?: boolean; restrictProbeCapabilities?: boolean }): Promise<Backend> {
  if (!Number.isSafeInteger(options.port) || options.port < 1024 || options.port > 65535) throw new BridgeError('BACKEND_PORT', 'Invalid backend port.');
  const endpoint = `ws://127.0.0.1:${options.port}`;
  const token = randomBytes(32).toString('base64url');
  const args = ['app-server', '--listen', endpoint, '--ws-auth', 'capability-token', '--ws-token-sha256', createHash('sha256').update(token).digest('hex')];
  // Only isolated protocol tests receive global restrictions. Normal desktop settings are inherited unchanged.
  if (options.isolated && options.restrictProbeCapabilities !== false) for (const key of ['apps', 'plugins', 'hooks', 'shell_tool', 'multi_agent', 'browser_use', 'computer_use', 'image_generation', 'goals', 'memories', 'skill_search', 'workspace_dependencies']) args.push('-c', `features.${key}=false`);
  if (options.isolated) args.push('-c', 'cli_auth_credentials_store="file"', '-c', 'features.secret_auth_storage=false');
  const env = cleanChildEnvironment();
  // Private loopback transports must connect directly, even when Windows has a system proxy.
  const inheritedNoProxy = Object.entries(env).filter(([key]) => key.toUpperCase() === 'NO_PROXY').flatMap(([, value]) => (value ?? '').split(',')).filter(Boolean);
  for (const key of Object.keys(env)) if (key.toUpperCase() === 'NO_PROXY') delete env[key];
  env.NO_PROXY = [...new Set([...inheritedNoProxy, '127.0.0.1', 'localhost', '::1'])].join(',');
  if (options.home) env.CODEX_HOME = options.home;
  if (options.isolated) for (const key of Object.keys(env)) if (/^(OPENAI|CHATGPT|AZURE_OPENAI|ANTHROPIC)_/i.test(key)) delete env[key];
  const child = spawn(options.executable, args, { cwd: options.cwd, env, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
  let ended = false;
  const exited = new Promise<void>(resolve => {
    const finish = () => { ended = true; resolve(); };
    child.once('exit', finish); child.once('error', finish);
  });
  // Discard stderr: config diagnostics can contain private paths, prompts, or credentials.
  child.stderr?.resume();
  const stop = async () => {
    if (ended) return;
    child.kill();
    let timer: NodeJS.Timeout | undefined;
    try { await Promise.race([exited, new Promise<void>(resolve => { timer = setTimeout(resolve, 5000); })]); }
    finally { clearTimeout(timer); }
    if (!ended) throw new BridgeError('BACKEND_STOP', 'The owned app-server did not exit; inspect its PID before restarting.');
  };
  try {
    const deadline = Date.now() + 15_000;
    while (!ended && Date.now() < deadline) {
      try {
        const rpc = await AppServerRpc.connect(endpoint, token, 10_000, 1500);
        try { await rpc.initialize(); } catch (error) { rpc.close(); throw error; }
        if (ended) { rpc.close(); break; }
        return { child, endpoint, token, rpc, exited, stop: async () => { rpc.close(); await stop(); } };
      } catch { if (!ended) await delay(200); }
    }
    throw new BridgeError('BACKEND_START', 'The owned authenticated app-server could not start or initialize.');
  } catch (error) { await stop(); throw error; }
}
