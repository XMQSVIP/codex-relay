import { useCallback, useEffect, useState } from 'react';
import { api, ApiError, initialToken } from './api';
import type { AdminSession, Overview } from './api';

export function useConsoleSession(stopAccepted: boolean) {
  const [token, setToken] = useState(initialToken);
  const [requiresToken, setRequiresToken] = useState<boolean | null>(null);
  const [session, setSession] = useState<AdminSession | null>(null);
  const [sessionError, setSessionError] = useState('');
  const [sessionAttempt, setSessionAttempt] = useState(0);
  const [data, setData] = useState<Overview | null>(null);
  const [connectionError, setConnectionError] = useState('');
  const [pending, setPending] = useState('');
  const [refresh, setRefresh] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const reload = useCallback(() => { setRefreshing(true); setRefresh(value => value + 1); }, []);
  const refreshAccounts = async () => {
    const next = await api<Overview>(token, '/overview');
    setData(next); setConnectionError('');
  };
  useEffect(() => {
    const controller = new AbortController();
    setSessionError('');
    void api<AdminSession>('', '/session', undefined, controller.signal).then(session => {
      if (controller.signal.aborted) return;
      if (typeof session.requiresToken !== 'boolean' || typeof session.passwordEnabled !== 'boolean' || typeof session.authenticated !== 'boolean') throw new Error('无法读取后台登录方式，请确认前后端版本一致。');
      if (!session.requiresToken) {
        setToken('');
        try { sessionStorage.removeItem('bridge-admin'); } catch { /* Storage may be unavailable. */ }
      }
      setRequiresToken(session.requiresToken);
      setSession(session);
    }).catch(cause => {
      if (!controller.signal.aborted) setSessionError(cause instanceof Error ? cause.message : '无法连接本机服务');
    });
    return () => controller.abort();
  }, [sessionAttempt]);
  const recheckSession = useCallback(() => {
    setData(null); setSession(null); setRequiresToken(null); setSessionAttempt(value => value + 1);
  }, []);
  useEffect(() => {
    window.addEventListener('bridge-auth-required', recheckSession);
    return () => window.removeEventListener('bridge-auth-required', recheckSession);
  }, [recheckSession]);
  useEffect(() => {
    if (stopAccepted || requiresToken === null || requiresToken && !token || !session?.authenticated) return;
    const controller = new AbortController(); let timer: ReturnType<typeof setTimeout>;
    const load = async () => {
      try { const next = await api<Overview>(token, '/overview', undefined, controller.signal); if (!controller.signal.aborted) { setData(next); setConnectionError(''); } }
      catch (error) {
        if (!controller.signal.aborted) {
          setConnectionError(error instanceof Error ? error.message : '无法连接本机服务');
          if (error instanceof ApiError && error.code === 'UNAUTHORIZED') {
            try { sessionStorage.removeItem('bridge-admin'); } catch { /* Memory-only session. */ }
            setToken(''); setData(null); setRequiresToken(null); setSessionAttempt(value => value + 1);
          }
        }
      }
      finally { if (!controller.signal.aborted) { setRefreshing(false); timer = setTimeout(load, 4000); } }
    };
    void load();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [token, requiresToken, session?.authenticated, refresh, stopAccepted]);

  return { token, setToken, requiresToken, session, setSession, sessionError, setSessionAttempt, data, connectionError, setConnectionError, pending, setPending, refresh, refreshing, reload, refreshAccounts, recheckSession };
}
