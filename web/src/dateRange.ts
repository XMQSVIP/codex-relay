export function localDate(value = new Date()): string {
  return `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, '0')}-${String(value.getDate()).padStart(2, '0')}`;
}

export function dateBounds(start: string, end: string): { from: number; to: number } | null {
  if (![start, end].every(value => /^\d{4}-\d{2}-\d{2}$/.test(value))) return null;
  const first = new Date(`${start}T00:00:00`);
  const last = new Date(`${end}T00:00:00`);
  if (localDate(first) !== start || localDate(last) !== end || !Number.isFinite(first.getTime()) || !Number.isFinite(last.getTime()) || first > last) return null;
  // Advancing the calendar day also handles 23/25-hour daylight-saving days.
  last.setDate(last.getDate() + 1);
  return { from: first.getTime(), to: last.getTime() - 1 };
}
