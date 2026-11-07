export interface ChatEvent {
  type: 'ready' | 'delta' | 'done' | 'error';
  text?: string; model?: string | null; requestId?: string; code?: string; message?: string;
  firstResponseMs?: number | null; totalMs?: number;
  usage?: { inputTokens: number; outputTokens: number; totalTokens: number } | null;
}

export async function readTestChat(stream: ReadableStream<Uint8Array>, receive: (event: ChatEvent) => void) {
  const reader = stream.getReader(), decoder = new TextDecoder();
  let buffer = '', terminal = false;
  try {
    while (!terminal) {
      const { value, done } = await reader.read();
      buffer += decoder.decode(value, { stream: !done });
      if (buffer.length > 2 * 1024 * 1024) throw new Error('测试响应过大，已停止。');
      let match: RegExpExecArray | null;
      while ((match = /\r?\n\r?\n/.exec(buffer))) {
        const block = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        const data = block.split(/\r?\n/).filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n');
        if (!data) continue;
        const event = JSON.parse(data) as ChatEvent;
        if (!['ready', 'delta', 'done', 'error'].includes(event.type)) throw new Error('测试响应格式无效。');
        receive(event);
        if (event.type === 'done' || event.type === 'error') { terminal = true; break; }
      }
      if (done) break;
    }
    if (!terminal) throw new Error('连接提前断开，未收到完整回复，请重试。');
  } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}
