import { useEffect, useState } from 'react';
import { Moon, Sun } from 'lucide-react';

type ThemePreference = 'light' | 'dark' | 'system';
// Keep the stored preference when upgrading from Codex Bridge to Codex Relay.
const storageKey = 'codex-bridge.theme.v1';
const normalize = (value: string | null | undefined): ThemePreference => value === 'light' || value === 'dark' ? value : 'system';

function applyTheme(preference: ThemePreference) {
  document.documentElement.dataset.themePreference = preference;
  document.documentElement.dataset.theme = preference === 'system'
    ? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : preference;
}

export function useTheme() {
  const [preference, setPreference] = useState(() => normalize(document.documentElement.dataset.themePreference));
  const [resolved, setResolved] = useState(() => document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light');

  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)');
    const syncTheme = (value: ThemePreference) => { applyTheme(value); setResolved(document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light'); };
    const onSystemChange = () => { if (preference === 'system') syncTheme('system'); };
    const onStorage = (event: StorageEvent) => {
      if (event.key === storageKey || event.key === null) {
        const next = normalize(event.newValue);
        syncTheme(next);
        setPreference(next);
      }
    };
    syncTheme(preference);
    media.addEventListener('change', onSystemChange);
    window.addEventListener('storage', onStorage);
    return () => {
      media.removeEventListener('change', onSystemChange);
      window.removeEventListener('storage', onStorage);
    };
  }, [preference]);

  const changePreference = (value: string) => {
    const next = normalize(value);
    applyTheme(next);
    setResolved(document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light');
    setPreference(next);
    try { localStorage.setItem(storageKey, next); } catch { /* Keep the selection for this page session. */ }
  };
  return { preference, resolved, changePreference };
}

export function ThemePicker({ resolved, changePreference }: ReturnType<typeof useTheme>) {
  const label = resolved === 'dark' ? '切换为浅色模式' : '切换为深色模式';
  const Icon = resolved === 'dark' ? Sun : Moon;
  return <button type="button" className="toolbar-icon theme-picker" aria-label={label} title={label} onClick={() => changePreference(resolved === 'dark' ? 'light' : 'dark')}><Icon size={19} aria-hidden="true" /></button>;
}
