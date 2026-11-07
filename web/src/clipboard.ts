// Some browsers leave clipboard permission requests pending. Always give the UI
// a chance to offer manual selection instead of waiting without feedback.
export async function copyText(value: string): Promise<void> {
  if (!navigator.clipboard) throw new Error('此浏览器无法自动复制，请手动选择文本。');
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      navigator.clipboard.writeText(value),
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error('复制未获浏览器授权，请手动选择文本。')), 2500); }),
    ]);
  } finally { clearTimeout(timer); }
}
