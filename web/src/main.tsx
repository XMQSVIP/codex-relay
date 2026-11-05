import { Component, StrictMode } from 'react';
import type { ErrorInfo, ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { ToastProvider } from './Toast';
import './style.css';

class Boundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  componentDidCatch(_error: Error, _info: ErrorInfo) { /* Account and token values must never enter browser logs. */ }
  render() { return this.state.failed ? <main className="fatal"><h1>界面未能完成加载</h1><p>刷新页面重试；账户进程仍由 Rust 服务管理。</p><button onClick={() => location.reload()}>重新加载</button></main> : this.props.children; }
}
createRoot(document.getElementById('root')!).render(<StrictMode><Boundary><ToastProvider><App /></ToastProvider></Boundary></StrictMode>);
