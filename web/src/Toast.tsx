import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { CheckCircle2, CircleAlert, Info, X } from 'lucide-react';
import './toast.css';

type Tone = 'success' | 'error' | 'info';
type Toast = { id: number; message: string; tone: Tone };
type Notify = (message: string, tone?: Tone) => void;
const ToastContext = createContext<Notify | null>(null);

export function useToast() {
  const notify = useContext(ToastContext);
  if (!notify) throw new Error('ToastProvider is required');
  return notify;
}

function ToastItem({ toast, dismiss }: { toast: Toast; dismiss: (id: number) => void }) {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const remaining = useRef(toast.tone === 'error' ? 8000 : 5000);
  useEffect(() => {
    if (hovered || focused) return;
    const started = Date.now();
    const timer = setTimeout(() => dismiss(toast.id), remaining.current);
    return () => { clearTimeout(timer); remaining.current = Math.max(0, remaining.current - (Date.now() - started)); };
  }, [toast.id, dismiss, hovered, focused]);
  const Icon = toast.tone === 'error' ? CircleAlert : toast.tone === 'info' ? Info : CheckCircle2;
  return <div className={`toast toast-${toast.tone}`} onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
    onFocusCapture={() => setFocused(true)} onBlurCapture={event => { if (!event.currentTarget.contains(event.relatedTarget)) setFocused(false); }}>
    <div className="toast-content" role={toast.tone === 'error' ? 'alert' : 'status'} aria-atomic="true"><Icon size={20} aria-hidden="true" /><span>{toast.message}</span></div>
    <button type="button" className="toast-close" aria-label="关闭操作提示" onClick={() => dismiss(toast.id)}><X size={17} aria-hidden="true" /></button>
  </div>;
}

function ToastViewport({ items, dismiss }: { items: Toast[]; dismiss: (id: number) => void }) {
  const viewport = useRef<HTMLDivElement>(null);
  const [target, setTarget] = useState<Element>(document.body);
  useLayoutEffect(() => {
    // Keep feedback interactive while a native modal makes the rest of the page inert.
    const updateTarget = () => setTarget(Array.from(document.querySelectorAll('dialog:modal')).at(-1) ?? document.body);
    updateTarget();
    const observer = new MutationObserver(updateTarget);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['open'] });
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const currentTarget = Array.from(document.querySelectorAll('dialog:modal')).at(-1) ?? document.body;
    if (currentTarget !== target) { setTarget(currentTarget); return; }
    const element = viewport.current;
    if (!element?.isConnected || !items.length) return;
    // The browser's top layer keeps notifications above dialog backdrops.
    if (element.matches(':popover-open')) element.hidePopover();
    element.showPopover();
  }, [items, target]);
  return createPortal(<div ref={viewport} className="toast-viewport" popover="manual" role="region" aria-label="操作提示">
    {items.map(toast => <ToastItem key={toast.id} toast={toast} dismiss={dismiss} />)}
  </div>, target);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const nextId = useRef(0);
  const dismiss = useCallback((id: number) => setItems(current => current.filter(item => item.id !== id)), []);
  const notify = useCallback<Notify>((message, tone = 'success') => {
    if (!message.trim()) return;
    const toast = { id: ++nextId.current, message, tone };
    setItems(current => [...current.filter(item => item.message !== message || item.tone !== tone).slice(-3), toast]);
  }, []);
  return <ToastContext.Provider value={notify}>{children}<ToastViewport items={items} dismiss={dismiss} /></ToastContext.Provider>;
}
