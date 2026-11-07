const fullNumber = new Intl.NumberFormat('zh-CN');
const shortNumber = new Intl.NumberFormat('en-US', { maximumFractionDigits: 2, useGrouping: false });

export function exactTokens(value: number | null | undefined, unknown = '未知') {
  return value == null || !Number.isFinite(value) ? unknown : fullNumber.format(value);
}

export function formatTokens(value: number | null | undefined, unknown = '未知') {
  if (value == null || !Number.isFinite(value)) return unknown;
  const magnitude = Math.abs(value);
  // Promote values that round to 1000M to the next unit.
  if (magnitude >= 999_995_000) return `${shortNumber.format(value / 1_000_000_000)}B`;
  // Promote values that round to 1000K to the next unit.
  if (magnitude >= 999_995) return `${shortNumber.format(value / 1_000_000)}M`;
  if (magnitude >= 1_000) return `${shortNumber.format(value / 1_000)}K`;
  return fullNumber.format(value);
}
