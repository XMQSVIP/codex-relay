import { BridgeError } from '../errors.js';

/** Candidate thread-only policy. Must pass live acceptance before use with external input. */
export function textOnlyConfig(mcpServers: string[]): Record<string, unknown> {
  const config: Record<string, unknown> = { web_search: 'disabled', project_doc_max_bytes: 0, 'features.skip_host_skill_discovery': true,
    'features.code_mode': { enabled: false, excluded_tool_namespaces: ['functions', 'clock'] } };
  for (const feature of [
    'apps', 'plugins', 'hooks', 'shell_tool', 'multi_agent', 'browser_use', 'computer_use',
    'image_generation', 'goals', 'memories', 'skill_search', 'workspace_dependencies', 'view_image',
    'sleep_tool', 'tool_suggest', 'browser_use_external', 'in_app_browser', 'code_mode_host',
    'code_mode_only', 'request_permissions_tool', 'unbounded_connection_retries',
  ]) config[`features.${feature}`] = false;
  for (const server of mcpServers) {
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(server)) throw new BridgeError('MCP_SERVER_NAME', 'Unsupported MCP configuration key; refusing to construct an incomplete deny policy.');
    config[`mcp_servers.${server}.enabled`] = false;
  }
  return config;
}
