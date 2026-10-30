import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { Activity, BarChart3, KeyRound, Layers3, LogOut, Menu, RefreshCw, Settings2, Users, X } from 'lucide-react';
import { ThemePicker, useTheme } from './theme';
import './console-shell.css';

export const pageTitles: Record<string, string> = { accounts: '账户管理', groups: '分组管理', keys: '客户端接入', requests: '请求监控', statistics: '用量统计', settings: '系统设置' };
const navigation = [
  { key: 'accounts', icon: Users }, { key: 'groups', icon: Layers3 }, { key: 'keys', icon: KeyRound },
  { key: 'requests', icon: Activity }, { key: 'statistics', icon: BarChart3 },
];

export function ConsoleShell({ view, theme, connected, failed, refreshing, reload, logout, accountCount, version, serviceControl, children }: {
  view: string; theme: ReturnType<typeof useTheme>; connected: boolean; failed: boolean; refreshing: boolean;
  reload: () => void; logout?: () => void; accountCount: number; version?: string; serviceControl: ReactNode; children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const toggle = useRef<HTMLButtonElement>(null);
  const sidebar = useRef<HTMLElement>(null);
  useEffect(() => {
    if (!open) return;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    sidebar.current?.querySelector<HTMLAnchorElement>('[aria-current="page"]')?.focus();
    const close = (event: KeyboardEvent) => {
      if (event.defaultPrevented || document.querySelector('dialog[open]')) return;
      if (event.key === 'Escape') { setOpen(false); toggle.current?.focus(); }
      if (event.key === 'Tab') {
        const items = [...(sidebar.current?.querySelectorAll<HTMLElement>('a,button:not(:disabled)') ?? [])];
        const first = items[0], last = items.at(-1);
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    };
    window.addEventListener('keydown', close);
    return () => { document.body.style.overflow = previousOverflow; window.removeEventListener('keydown', close); };
  }, [open]);
  useEffect(() => { const media = matchMedia('(min-width: 761px)'); const close = () => { if (media.matches) setOpen(false); }; media.addEventListener('change', close); return () => media.removeEventListener('change', close); }, []);
  useEffect(() => { document.title = `${pageTitles[view]} · Codex Relay`; }, [view]);
  return <div className={`app-shell${open ? ' navigation-open' : ''}`}>
    <a href="#main" className="skip-link">跳到主要内容</a>
    {open ? <button className="navigation-backdrop" aria-label="关闭导航菜单" onClick={() => { setOpen(false); toggle.current?.focus(); }} /> : null}
    <aside ref={sidebar} id="main-navigation" className="sidebar" aria-label="主导航" role={open ? 'dialog' : undefined} aria-modal={open || undefined} onClick={event => { if ((event.target as Element).closest('a')) setOpen(false); }}>
      <div className="sidebar-content">
        <a className="brand" href="?view=accounts"><img className="brand-symbol" src="/relay-icon-128.png" width={34} height={34} alt="" aria-hidden="true" /><span>Codex Relay<small>模型接入管理台</small></span></a>
        <nav aria-label="工作台导航"><span className="nav-section">接入管理</span>{navigation.map((item, index) => <div className="nav-item" key={item.key}>{index === 3 ? <span className="nav-section">运行监控</span> : null}<a href={`?view=${item.key}`} aria-current={view === item.key ? 'page' : undefined}><item.icon size={18} aria-hidden="true" />{pageTitles[item.key]}{item.key === 'accounts' ? <span className="nav-count">{accountCount}</span> : null}</a></div>)}</nav>
        <div className="sidebar-bottom"><a className="settings-link" href="?view=settings" aria-current={view === 'settings' ? 'page' : undefined}><Settings2 size={18} aria-hidden="true" />系统设置</a><div className="sidebar-footer"><div className="sidebar-version"><span className={`status-dot ${failed ? 'warning' : connected ? 'success' : ''}`} aria-hidden="true" /><span>本机中转站</span><small>v{version ?? '0.2.0'}</small></div>{serviceControl}</div></div>
      </div>
    </aside>
    <div className="workspace"><header className="topbar"><div className="workspace-title"><button ref={toggle} className="toolbar-icon mobile-menu-toggle" type="button" aria-label={open ? '收起导航菜单' : '展开导航菜单'} aria-expanded={open} aria-controls="main-navigation" onClick={() => setOpen(value => !value)}>{open ? <X size={20} /> : <Menu size={20} />}</button><span>{pageTitles[view]}</span></div><div className="topbar-actions"><span className={`service-connection ${failed ? 'warning' : ''}`} role="status"><span className="status-dot" aria-hidden="true" />{failed ? '连接异常' : connected ? '服务已连接' : '正在连接'}</span><button className="toolbar-icon" aria-label="刷新状态" title="刷新状态" disabled={refreshing} onClick={reload}><RefreshCw size={18} className={refreshing ? 'spin' : ''} aria-hidden="true" /></button><ThemePicker {...theme} />{logout ? <button className="toolbar-icon" aria-label="退出管理会话" title="退出管理会话" onClick={logout}><LogOut size={18} aria-hidden="true" /></button> : null}</div></header><main id="main" tabIndex={-1} className={`console-page ${view}-page`}><h1 className="visually-hidden">{pageTitles[view]}</h1>{children}</main></div>
  </div>;
}
