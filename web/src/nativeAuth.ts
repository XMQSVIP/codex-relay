/** Safe native authentication metadata; credentials stay in app-server. */
export function nativeAuthLabel(auth?: string): string {
  if (!auth) return '刷新后识别认证方式';
  if (auth.startsWith('native-chatgpt')) return '本机 ChatGPT 账户登录';
  if (auth === 'native-api-key') return '本机 API Key';
  if (auth === 'native-bearer') return '提供方 API Key / Bearer Token';
  if (auth === 'native-headers') return '提供方请求头认证';
  if (auth === 'native-provider-auth') return '提供方原生认证';
  return auth === 'none' ? '无需认证' : '原生 Codex 认证';
}
