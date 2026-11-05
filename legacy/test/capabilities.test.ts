import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inspectNativeCapabilities, releaseGate } from '../src/capabilities.js';
import { selectDesktop } from '../src/doctor.js';
import { validatePipeName, type DesktopProcess, type WindowsSnapshot } from '../src/desktop/windows.js';

test('native read/create/send capabilities cannot satisfy inference acceptance', () => {
  const tools = ['create_thread', 'send_message_to_thread', 'read_thread', 'wait_threads'].map(name => ({ name }));
  const checks = inspectNativeCapabilities(tools);
  assert.equal(checks.find(c => c.id === 'native_chat_tools')?.state, 'passed');
  assert.equal(checks.find(c => c.id === 'native_permission_controls')?.state, 'blocked');
  assert.equal(releaseGate(checks).ready, false);
  assert.equal(releaseGate(checks).missing.length, 5);
  const withConfig = inspectNativeCapabilities([{ name: 'create_thread', inputSchema: { properties: { config: {} } } }]);
  assert.equal(withConfig.find(c => c.id === 'native_permission_controls')?.state, 'unverified');
});

test('select the desktop main process even when the backend is shared, and reject ambiguity', () => {
  const proc = (pid: number, parentPid: number, name: string, isAppServer = false): DesktopProcess => ({ pid, parentPid, name, isAppServer, executable: isAppServer ? 'C:\\CLI\\codex.exe' : 'C:\\Program Files\\WindowsApps\\OpenAI.Codex_26\\app\\ChatGPT.exe' });
  const snapshot: WindowsSnapshot = { processes: [proc(10, 1, 'ChatGPT.exe'), proc(11, 10, 'codex.exe', true), proc(12, 1, 'codex.exe', true)], pipes: [], listeners: [] };
  assert.equal(selectDesktop(snapshot)?.pid, 10);
  snapshot.processes = snapshot.processes.filter(p => p.pid !== 11);
  snapshot.processes.push(proc(13, 10, 'ChatGPT.exe'));
  assert.equal(selectDesktop(snapshot)?.pid, 10);
  snapshot.processes.push(proc(20, 1, 'ChatGPT.exe'), proc(21, 20, 'codex.exe', true));
  assert.equal(selectDesktop(snapshot), undefined);
  assert.equal(selectDesktop(snapshot, 20)?.pid, 20);
  assert.equal(selectDesktop(snapshot, 12), undefined);
});

test('native pipe discovery rejects browser pipes and remote hosts', () => {
  validatePipeName('\\\\.\\pipe\\codex-app-tools-1234');
  for (const pipe of ['\\\\remote\\pipe\\codex-app-tools-1', '\\\\.\\pipe\\codex-browser-use-1', 'C:\\file', '\\\\.\\pipe\\codex-app-tools-..\\escape']) {
    assert.throws(() => validatePipeName(pipe), { code: 'PIPE_NAME' });
  }
});
