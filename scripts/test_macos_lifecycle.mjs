/** Native lifecycle acceptance on a running local bridge. Opens threads, never starts model turns.
 * node scripts/test_macos_lifecycle.mjs --account UUID --shutdown api|sigterm
 * Shuts down this bridge after checking disconnect and instance stop. Never stops the desktop.
 */
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { parseArgs } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import WebSocket from 'ws';
const { values } = parseArgs({ options: { account: { type: 'string' }, shutdown: { type: 'string' }, port: { type: 'string', default: '4317' } } });
assert.equal(process.platform, 'darwin');
assert(values.account && ['api', 'sigterm'].includes(values.shutdown), 'Specify --account and --shutdown api|sigterm.');
const base = `http://127.0.0.1:${Number(values.port)}`;
const management = process.env.BRIDGE_ADMIN_TOKEN;
async function api(path, body) {
  const r = await fetch(base + path, { method: body === undefined ? 'GET' : 'POST', headers: { 'content-type': 'application/json', ...(management ? { authorization: `Bearer ${management}` } : {}) }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30000) });
  const value = await r.json(); assert(r.ok, `Management HTTP ${r.status}: ${value.error?.code}`); return value;
}
function processes() {
  return execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,stat=,comm='], { encoding: 'utf8' }).trim().split('\n').map(line => {
    const m = line.match(/^\s*(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/);
    return m && { pid: Number(m[1]), parent: Number(m[2]), state: m[3], executable: m[4] };
  }).filter(Boolean);
}
const listenerPids = [...new Set(execFileSync('/usr/sbin/lsof', [`-tiTCP:${Number(values.port)}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).trim().split('\n').map(Number))];
assert.equal(listenerPids.length, 1, 'Require a unique local bridge listener.');
const bridge = listenerPids[0];
const before = processes();
assert(before.find(p => p.pid === bridge)?.executable.endsWith('codex-relay'), 'Listener must be the bridge executable.');
const desktops = before.filter(p => /\/(ChatGPT|Codex)\.app\/Contents\/MacOS\/(ChatGPT|Codex)$/.test(p.executable));
const personal = before.filter(p => desktops.some(d => p.parent === d.pid) && p.executable.endsWith('/codex'));
assert(desktops.length && personal.length, 'Keep the personal Codex desktop running.');
function descendants() {
  const all = processes(), found = new Set([bridge]); let previous;
  do { previous = found.size; for (const p of all) if (found.has(p.parent)) found.add(p.pid); } while (found.size !== previous);
  found.delete(bridge); return [...found];
}
async function until(check, message) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) { if (await check()) return; await new Promise(r => setTimeout(r, 50)); }
  throw new Error(message);
}
async function gone(pids) { await until(() => !processes().some(p => pids.includes(p.pid) && !p.state.startsWith('Z')), 'Owned native process survived cleanup.'); }
async function idle() { await until(async () => { const s = await api('/api/operations'); return s.active === 0 && s.queued === 0; }, 'Admission slot remained active.'); }
let key, stopped = false;
const report = { modelTurns: 0, tests: [], shutdown: values.shutdown, completed: false };
const pass = name => { report.tests.push(name); console.log(`PASS: ${name}`); };
let activeSocket;
try {
  const operations = await api('/api/operations');
  const account = operations.accounts.find(a => a.id === values.account);
  assert(account?.route);
  key = await api('/api/api-keys', { name: 'macOS lifecycle acceptance (temporary)', groupId: account.groupIds[0], models: [account.route.model], maxConcurrency: 1 });
  async function session() {
    const socket = new WebSocket(base.replace('http:', 'ws:') + '/v1/native', { headers: { authorization: `Bearer ${key.secret}` } });
    activeSocket = socket;
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Thread initialization timed out.')), 45000);
      socket.on('error', reject);
      socket.on('open', () => socket.send(JSON.stringify({ id: 1, method: 'thread/start', params: { model: account.route.model } })));
      socket.on('message', bytes => { const e = JSON.parse(bytes.toString()); if (e.id === 1 && e.result) { clearTimeout(timer); resolve(); } else if (e.method === 'bridge/error') { clearTimeout(timer); reject(new Error(`${e.params.code}: ${e.params.message}`)); } });
    });
    const owned = descendants(); assert(owned.length > 0, 'Native child must exist before cleanup.');
    return { socket, owned };
  }
  let s = await session(); s.socket.terminate(); await idle(); await gone(s.owned); pass('client disconnect cancels native session and reaps owned processes');
  s = await session(); await api(`/api/accounts/${values.account}/stop`, {}); await idle(); await gone(s.owned); s.socket.terminate(); pass('administrator instance stop reaps owned processes');
  s = await session();
  // Revoke the temporary Key before shutdown; this does not log out the OAuth account.
  await api(`/api/api-keys/${key.key.id}`, { revision: key.key.revision, state: 'revoked' }); key = null;
  if (values.shutdown === 'api') await api('/api/service/stop', {});
  else process.kill(bridge, 'SIGTERM');
  stopped = true;
  await gone([bridge, ...s.owned]); s.socket.terminate();
  pass(`${values.shutdown} service exit reaps owned processes`);
  const after = processes();
  for (const p of [...desktops, ...personal]) assert(after.some(a => a.pid === p.pid && a.executable === p.executable), 'Personal desktop process was affected.');
  pass('personal desktop and its native backend remain running');
  report.completed = true;
} finally {
  activeSocket?.terminate();
  if (key && !stopped) await api(`/api/api-keys/${key.key.id}`, { revision: key.key.revision, state: 'revoked' });
  await mkdir('.cache', { recursive: true });
  await writeFile(`.cache/macos-lifecycle-${values.shutdown}.json`, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
}
