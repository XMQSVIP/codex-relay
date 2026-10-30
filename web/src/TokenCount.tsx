import { exactTokens, formatTokens } from './tokenFormat';

export function TokenCount({ value, unknown = '未知' }: { value: number | null | undefined; unknown?: string }) {
  return <span title={value != null && Number.isFinite(value) ? `${exactTokens(value)} Token` : undefined}>{formatTokens(value, unknown)}</span>;
}
