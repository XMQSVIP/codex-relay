import { setTimeout as delay } from 'node:timers/promises';
import { z } from 'zod';
import { BridgeError } from '../errors.js';
import type { AppServerRpc } from '../transport/rpc.js';
import type { startLanTextGuard } from '../guard/loopback-guard.js';
import { guardedProviderOverrides, type LanProvider } from './provider.js';
import { textOnlyConfig } from './text-policy.js';

export type TextGuard = Awaited<ReturnType<typeof startLanTextGuard>>;

/** Fixed diagnostic turns only. No API caller can choose provider, configuration, cwd, or RPC methods. */
export async function runGuardedCheck(options: {
  rpc: AppServerRpc; guard: TextGuard; provider: LanProvider; cwd: string;
  prompt: string; interruptAfterFirstDelta?: boolean; timeoutMs?: number;
}) {
  const { rpc, guard, provider } = options;
  const route = guard.createRoute(provider.model);
  let threadId: string | undefined; let turnId: string | undefined; let observedTurnId: string | undefined;
  let firstDeltaAt: number | null = null; let terminalAt: number | null = null;
  let deltaCount = 0; let textCharacters = 0; let toolItems = 0;
  let interruptRequested = false; let interruptAcknowledged = false;
  let interruptPromise: Promise<void> | undefined;
  const agentItems = new Set<string>();
  let unsubscribe: (() => void) | undefined;
  let timer: NodeJS.Timeout | undefined;
  let terminalSeen = false;
  try {
    const started = z.object({ thread: z.object({ id: z.string() }), model: z.literal(provider.model), modelProvider: z.literal(provider.id),
      approvalPolicy: z.literal('never'), sandbox: z.object({ type: z.literal('readOnly') }),
    }).safeParse(await rpc.request('thread/start', {
      modelProvider: provider.id, model: provider.model, allowProviderModelFallback: false, cwd: options.cwd, ephemeral: true,
      approvalPolicy: 'never', sandbox: 'read-only', dynamicTools: [], environments: [], selectedCapabilityRoots: [], runtimeWorkspaceRoots: [],
      config: { ...textOnlyConfig(provider.mcpServers), ...guardedProviderOverrides(provider, route.baseUrl) },
      developerInstructions: 'Answer the user with plain text only. This is a text-only service conversation.',
    }));
    if (!started.success) throw new BridgeError('CHECK_THREAD_POLICY', 'The native server did not acknowledge the selected model, provider, or permission policy.');
    threadId = started.data.thread.id;
    let complete: (status: string) => void = () => {};
    let fail: (error: Error) => void = () => {};
    const terminal = new Promise<string>((resolve, reject) => { complete = resolve; fail = reject; });
    void terminal.catch(() => {});
    timer = setTimeout(() => fail(new BridgeError('CHECK_TIMEOUT', 'The diagnostic turn did not reach a confirmed terminal state.')), options.timeoutMs ?? 90_000);
    const correlate = (id: string) => {
      if ((turnId !== undefined && turnId !== id) || (observedTurnId !== undefined && observedTurnId !== id)) throw new BridgeError('CHECK_CORRELATION', 'Native turn identifiers did not match.');
      observedTurnId = id;
    };
    const interrupt = () => {
      if (!options.interruptAfterFirstDelta || !firstDeltaAt || !turnId || interruptRequested) return;
      interruptRequested = true;
      interruptPromise = rpc.request('turn/interrupt', { threadId, turnId }).then(() => { interruptAcknowledged = true; }, () => {
        fail(new BridgeError('CHECK_INTERRUPT', 'Native interrupt was not acknowledged.'));
      });
    };
    unsubscribe = rpc.subscribe(event => {
      try {
        const scope = z.object({ threadId: z.string() }).safeParse(event.params);
        if (!scope.success || scope.data.threadId !== threadId) return;
        if (event.method === 'turn/started' || event.method === 'turn/completed') {
          const params = z.object({ turn: z.object({ id: z.string(), status: z.string() }) }).parse(event.params);
          correlate(params.turn.id);
          if (event.method === 'turn/completed') { terminalSeen = true; terminalAt = Date.now(); complete(params.turn.status); }
        } else if (event.method === 'item/started' || event.method === 'item/completed') {
          const params = z.object({ turnId: z.string(), item: z.object({ id: z.string(), type: z.string() }) }).parse(event.params);
          correlate(params.turnId);
          if (params.item.type === 'agentMessage') agentItems.add(params.item.id);
          else if (!['userMessage', 'reasoning'].includes(params.item.type)) { toolItems++; fail(new BridgeError('CHECK_TOOL_ITEM', 'A tool item appeared in the protected diagnostic turn.')); }
        } else if (event.method === 'item/agentMessage/delta') {
          const params = z.object({ turnId: z.string(), itemId: z.string(), delta: z.string() }).parse(event.params);
          correlate(params.turnId);
          if (!agentItems.has(params.itemId) || terminalSeen) throw new BridgeError('CHECK_CORRELATION', 'Text delta has no active matching assistant item.');
          if (params.delta.length) { firstDeltaAt ??= Date.now(); deltaCount++; textCharacters += params.delta.length; interrupt(); }
        }
      } catch (error) { fail(error instanceof BridgeError ? error : new BridgeError('CHECK_PROTOCOL', 'A diagnostic event did not match the installed protocol.')); }
    });
    const turn = z.object({ turn: z.object({ id: z.string() }) }).parse(await rpc.request('turn/start', {
      threadId, input: [{ type: 'text', text: options.prompt, text_elements: [] }],
    }));
    turnId = turn.turn.id; correlate(turnId); interrupt();
    const status = await terminal;
    await interruptPromise;
    // Native cancellation may close HTTP a few event-loop iterations after its terminal notification.
    const settleDeadline = Date.now() + 2500;
    while (route.snapshot().state === 'active' && Date.now() < settleDeadline) await delay(25);
    return { threadId, turnId, itemIds: [...agentItems], status, deltaCount, textCharacters, toolItems,
      firstDeltaAt, terminalAt, interruptRequested, interruptAcknowledged,
      guard: route.snapshot(), exactCorrelation: observedTurnId === turnId && agentItems.size > 0,
    };
  } catch (error) {
    if (threadId && turnId && !terminalSeen) await rpc.request('turn/interrupt', { threadId, turnId }).catch(() => {});
    throw error;
  } finally {
    clearTimeout(timer); unsubscribe?.();
    if (route.snapshot().state !== 'active') route.release();
  }
}
