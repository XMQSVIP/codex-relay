// Run before styles and React so a saved dark theme never flashes a light page.
(() => {
  let preference = 'system';
  try {
    // Stable storage key preserves themes set before the Codex Relay rename.
    const saved = localStorage.getItem('codex-bridge.theme.v1');
    if (saved === 'light' || saved === 'dark') preference = saved;
  } catch { /* Storage may be unavailable; following the system still works. */ }
  document.documentElement.dataset.themePreference = preference;
  document.documentElement.dataset.theme = preference === 'system'
    ? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : preference;
})();
