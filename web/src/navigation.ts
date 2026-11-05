import { useLayoutEffect, useSyncExternalStore } from 'react';
import type { MouseEvent, SetStateAction } from 'react';

const subscribe = (callback: () => void) => {
  window.addEventListener('popstate', callback);
  window.addEventListener('console-navigation', callback);
  return () => { window.removeEventListener('popstate', callback); window.removeEventListener('console-navigation', callback); };
};
const snapshot = () => location.search;
const positions = new Map<string, number>();
let restorePosition = false;

export function goTo(href: string, replace = false) {
  const url = new URL(href, location.href);
  if (url.origin !== location.origin || url.pathname !== location.pathname) return;
  if (url.href === location.href) return;
  positions.set(location.search, window.scrollY);
  history[replace ? 'replaceState' : 'pushState'](null, '', url);
  restorePosition = false;
  window.dispatchEvent(new Event('console-navigation'));
}

export function useQueryState(key: string, fallback: string, replace = true): [string, (value: SetStateAction<string>) => void] {
  const search = useSyncExternalStore(subscribe, snapshot);
  const value = (new URLSearchParams(search).get(key) ?? fallback);
  return [value, next => {
    const params = new URLSearchParams(location.search);
    const resolved = typeof next === 'function' ? next((params.get(key) ?? fallback)) : next;
    if (resolved === fallback) params.delete(key); else params.set(key, resolved);
    goTo(`?${params}`, replace);
  }];
}

export function useQueryNumber(key: string, fallback: number): [number, (value: SetStateAction<number>) => void] {
  const [raw, setRaw] = useQueryState(key, String(fallback), false);
  const parsed = Number(raw);
  const value = Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
  return [value, next => setRaw(String(typeof next === 'function' ? next(value) : next))];
}

export function accountListUrl(id?: string) {
  const params = new URLSearchParams(location.search);
  params.set('view', 'accounts'); params.delete('account'); params.delete('section');
  return `?${params}${id ? `#account-${id}` : ''}`;
}

export function useConsoleNavigation() {
  const search = useSyncExternalStore(subscribe, snapshot);
  const params = new URLSearchParams(search);
  const pageKey = [params.get('view'), params.get('account'), params.get('tab'), params.get('section')].join(':');
  useLayoutEffect(() => {
    const back = () => { restorePosition = true; };
    window.addEventListener('popstate', back);
    return () => window.removeEventListener('popstate', back);
  }, []);
  useLayoutEffect(() => {
    if (restorePosition) { window.scrollTo(0, positions.get(location.search) ?? 0); restorePosition = false; return; }
    const target = location.hash.startsWith('#account-') ? document.getElementById(location.hash.slice(1)) : null;
    if (target) { target.focus({ preventScroll: true }); target.scrollIntoView({ block: 'center' }); }
    else { document.getElementById('main')?.focus({ preventScroll: true }); window.scrollTo(0, 0); }
  }, [pageKey]);
  const navigate = (event: MouseEvent<HTMLElement>) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const link = (event.target as Element).closest<HTMLAnchorElement>('a[href]');
    if (!link || link.target || link.hasAttribute('download')) return;
    const url = new URL(link.href);
    if (url.origin !== location.origin || url.pathname !== location.pathname || !url.searchParams.has('view')) return;
    event.preventDefault();
    if (url.href !== location.href) goTo(url.href);
  };
  return { search, navigate };
}
