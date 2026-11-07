import { open, type FileHandle } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { z } from 'zod';
import { BridgeError } from '../errors.js';

const nodeSchema = z.object({
  files: z.record(z.string(), z.unknown()).optional(),
  offset: z.string().optional(),
  size: z.number().int().nonnegative().optional(),
  unpacked: z.boolean().optional(),
});

async function readExactly(file: FileHandle, count: number, offset: number): Promise<Buffer> {
  const result = Buffer.alloc(count);
  let used = 0;
  while (used < count) {
    const { bytesRead } = await file.read(result, used, count - used, offset + used);
    if (!bytesRead) throw new BridgeError('ASAR_TRUNCATED', 'Application archive is truncated.');
    used += bytesRead;
  }
  return result;
}

export interface SourceInspection {
  archive: string;
  files: Array<{ path: string; sha256: string; requestAnchor: boolean; eventAnchors: string[]; sharedWebSocketOverride: boolean }>;
  note: string;
}

/** Read shipped JS as data only. Never extract, evaluate, or modify application code. */
export async function inspectArchive(archive: string): Promise<SourceInspection> {
  const file = await open(archive, 'r');
  try {
    const stat = await file.stat();
    const prefix = await readExactly(file, 16, 0);
    const headerSize = prefix.readUInt32LE(4);
    const jsonSize = prefix.readUInt32LE(12);
    if (prefix.readUInt32LE(0) !== 4 || headerSize < 8 || headerSize > 32 * 1024 * 1024
        || jsonSize > headerSize - 8 || 8 + headerSize > stat.size) {
      throw new BridgeError('ASAR_HEADER', 'Unrecognized application archive header.');
    }
    const header: unknown = JSON.parse((await readExactly(file, jsonSize, 16)).toString('utf8'));
    const entries: Array<{ path: string; offset: number; size: number }> = [];
    const visit = (value: unknown, parent: string, depth: number): void => {
      if (depth > 32) throw new BridgeError('ASAR_DEPTH', 'Application archive exceeds nesting limit.');
      const node = nodeSchema.parse(value);
      if (node.files) {
        for (const [name, child] of Object.entries(node.files)) {
          if (name.includes('/') || name.includes('\\') || name === '..') throw new BridgeError('ASAR_PATH', 'Invalid archive entry name.');
          visit(child, parent ? `${parent}/${name}` : name, depth + 1);
        }
      } else if ((/^webview\/assets\/app-(initial|shared)-[^/]+\.js$/.test(parent)
        || /^\.vite\/build\/(application-network-startup|main)-[^/]+\.js$/.test(parent)) && !node.unpacked) {
        const offset = Number(node.offset);
        const size = node.size ?? -1;
        if (!Number.isSafeInteger(offset) || offset < 0 || size < 0 || size > 32 * 1024 * 1024
            || offset + size + 8 + headerSize > stat.size) throw new BridgeError('ASAR_OFFSET', 'Invalid archive entry bounds.');
        entries.push({ path: parent, offset, size });
      }
    };
    visit(header, '', 0);
    if (entries.length > 8) throw new BridgeError('ASAR_AMBIGUOUS', 'Too many application bundle candidates.');
    const files: SourceInspection['files'] = [];
    for (const entry of entries) {
      const bytes = await readExactly(file, entry.size, 8 + headerSize + entry.offset);
      const source = bytes.toString('utf8');
      files.push({
        path: entry.path,
        sha256: createHash('sha256').update(bytes).digest('hex'),
        requestAnchor: source.includes('AppServerRequestClient is missing a message dispatcher'),
        eventAnchors: ['item/agentMessage/delta', 'turn/completed', 'thread/tokenUsage/updated'].filter(key => source.includes(key)),
        sharedWebSocketOverride: source.includes('CODEX_APP_SERVER_WS_URL') && source.includes('CODEX_APP_SERVER_FORCE_CLI'),
      });
    }
    return { archive, files, note: 'Static source anchors are compatibility clues only, not proof of a live connection or tool isolation.' };
  } finally { await file.close(); }
}
