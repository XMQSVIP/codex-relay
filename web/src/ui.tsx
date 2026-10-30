import { useEffect, useId, useRef } from 'react';
import type { ReactNode } from 'react';
import { X } from 'lucide-react';

export function Tabs({ items, label }: { items: { href: string; label: string; active: boolean }[]; label: string }) {
  return <nav className="page-tabs" aria-label={label}>{items.map(item => <a key={item.href} href={item.href} aria-current={item.active ? 'page' : undefined}>{item.label}</a>)}</nav>;
}

export function Modal({ title, close, busy = false, trapFocus = false, children }: { title: string; close: () => void; busy?: boolean; trapFocus?: boolean; children: ReactNode }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const heading = useId();
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const element = dialog.current;
    element?.showModal();
    return () => { element?.close(); if (previous?.isConnected) previous.focus(); };
  }, []);
  return <dialog ref={dialog} className="standard-dialog" aria-labelledby={heading} onKeyDown={event => {
    if (!trapFocus || event.key !== 'Tab') return;
    const controls = [...event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href], [tabindex="0"]')].filter(node => node.getClientRects().length);
    const first = controls[0], last = controls.at(-1);
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
  }} onCancel={event => { event.preventDefault(); if (!busy) close(); }}>
    <div className="section-heading"><h2 id={heading}>{title}</h2><button className="toolbar-icon" aria-label="关闭窗口" type="button" disabled={busy} onClick={close}><X size={18} /></button></div>{children}
  </dialog>;
}
