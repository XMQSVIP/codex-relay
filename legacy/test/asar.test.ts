import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, unlink, rmdir } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { inspectArchive } from '../src/desktop/asar.js';

function archiveBytes(source: string, offset = '0'): Buffer {
  const body = Buffer.from(source);
  const header = Buffer.from(JSON.stringify({ files: { webview: { files: { assets: { files: {
    'app-initial-example.js': { offset, size: body.length },
  } } } } } }));
  const aligned = Math.ceil((header.length + 8) / 4) * 4;
  const prefix = Buffer.alloc(8 + aligned);
  prefix.writeUInt32LE(4, 0); prefix.writeUInt32LE(aligned, 4);
  prefix.writeUInt32LE(aligned - 4, 8); prefix.writeUInt32LE(header.length, 12);
  header.copy(prefix, 16);
  return Buffer.concat([prefix, body]);
}

test('ASAR source inspection is bounded and returns only hashes and known anchors', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'codex-bridge-asar-'));
  const filename = path.join(dir, 'app.asar');
  try {
    const source = 'AppServerRequestClient is missing a message dispatcher; item/agentMessage/delta; turn/completed; SECRET_SOURCE';
    await writeFile(filename, archiveBytes(source));
    const report = await inspectArchive(filename);
    assert.equal(report.files[0]?.requestAnchor, true);
    assert.deepEqual(report.files[0]?.eventAnchors, ['item/agentMessage/delta', 'turn/completed']);
    assert.match(report.files[0]!.sha256, /^[a-f0-9]{64}$/);
    assert.equal(JSON.stringify(report).includes('SECRET_SOURCE'), false);
    await writeFile(filename, archiveBytes(source, '999999999'));
    await assert.rejects(inspectArchive(filename), { code: 'ASAR_OFFSET' });
    await writeFile(filename, Buffer.alloc(16));
    await assert.rejects(inspectArchive(filename), { code: 'ASAR_HEADER' });
  } finally { await unlink(filename).catch(() => {}); await rmdir(dir); }
});
