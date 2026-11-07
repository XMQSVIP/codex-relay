import type { NativeTool } from './transport/native.js';

export type CheckState = 'passed' | 'blocked' | 'unverified';
export interface Check { id: string; state: CheckState; detail: string }

export function inspectNativeCapabilities(tools: NativeTool[]): Check[] {
  const names = new Set(tools.map(t => t.name));
  const create = tools.find(t => t.name === 'create_thread');
  const properties = create?.inputSchema?.properties;
  const fields = properties && typeof properties === 'object' ? Object.keys(properties) : [];
  const hasPermissions = fields.some(key => ['config', 'permissions', 'sandbox', 'tools'].includes(key));
  return [
    { id: 'native_chat_tools', state: ['create_thread', 'send_message_to_thread', 'read_thread', 'wait_threads'].every(n => names.has(n)) ? 'passed' : 'blocked', detail: 'Read-only tools/list inspection; no conversation was created or messaged.' },
    { id: 'native_stream_api', state: 'unverified', detail: 'The native tool listing is not a token event subscription. Live text deltas must be demonstrated separately.' },
    { id: 'native_permission_controls', state: hasPermissions ? 'unverified' : 'blocked', detail: hasPermissions ? 'Permission fields exist; their enforcement has not been tested.' : 'create_thread exposes no per-request permission/tool configuration. Do not dispatch untrusted prompts through this path.' },
  ];
}

export function releaseGate(checks: Check[]): { ready: boolean; missing: string[] } {
  const required = ['same_desktop_instance', 'live_text_before_completion', 'exact_turn_correlation', 'confirmed_interrupt', 'tools_disabled_before_dispatch'];
  const missing = required.filter(id => !checks.some(c => c.id === id && c.state === 'passed'));
  return { ready: missing.length === 0, missing };
}
