import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { Statistics } from '../api';
import { dateBounds } from '../dateRange';
import { goTo } from '../navigation';

export const STATISTICS_REFRESH_SECONDS = 30;

export interface StatisticsExtraFilters {
  keyId: string;
  model: string;
  errorCode: string;
  keyName: string;
  accountName: string;
  groupName: string;
}

interface StatisticsQuery {
  token: string;
  startDate: string;
  endDate: string;
  groupId: string;
  accountId: string;
  kind: string;
  status: string;
  page: number;
  pageSize: number;
  refreshVersion: number;
  extra: StatisticsExtraFilters;
}

// Each query owns its controller and next poll. Changing filters, clearing
// records or unmounting must prevent an older response from restoring stale data.
export function useStatisticsData({
  token,
  startDate,
  endDate,
  groupId,
  accountId,
  kind,
  status,
  page,
  pageSize,
  refreshVersion,
  extra: { keyId, model, errorCode, keyName, accountName, groupName },
}: StatisticsQuery) {
  const statisticsRequest = useRef<AbortController | null>(null);
  const [manualRefresh, setManualRefresh] = useState(0);
  const [recordsVersion, setRecordsVersion] = useState(0);
  const refreshSeconds = STATISTICS_REFRESH_SECONDS;
  const [data, setData] = useState<Statistics | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    const controller = new AbortController();
    statisticsRequest.current = controller;
    let timer: ReturnType<typeof setTimeout>;
    setData(null);
    setLoading(true);
    const bounds = dateBounds(startDate, endDate);
    if (!bounds) {
      setError('日期范围无效，请重新选择开始和结束日期。');
      setLoading(false);
      return () => controller.abort();
    }
    const params = new URLSearchParams({
      from: String(bounds.from),
      to: String(bounds.to),
      page: String(page),
      pageSize: String(pageSize),
    });
    for (const [key, value] of Object.entries({
      groupId,
      accountId,
      kind,
      status,
      keyId,
      model,
      errorCode,
      keyName,
      accountName,
      groupName,
    }))
      if (value) params.set(key, value);
    const load = async () => {
      try {
        const next = await api<Statistics>(
          token,
          `/statistics?${params}`,
          undefined,
          controller.signal,
        );
        if (!controller.signal.aborted) {
          setData(next);
          setError('');
          if (next.page !== page) {
            const url = new URLSearchParams(location.search);
            url.set('page', String(next.page));
            goTo(`?${url}`, true);
          }
        }
      } catch (e) {
        if (!controller.signal.aborted) setError(e instanceof Error ? e.message : '统计加载失败');
      } finally {
        if (!controller.signal.aborted) {
          setLoading(false);
          timer = setTimeout(load, refreshSeconds * 1000);
        }
      }
    };
    void load();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [
    token,
    startDate,
    endDate,
    refreshSeconds,
    manualRefresh,
    groupId,
    accountId,
    kind,
    status,
    page,
    pageSize,
    refreshVersion,
    keyId,
    model,
    errorCode,
    keyName,
    accountName,
    groupName,
    recordsVersion,
  ]);
  const invalidateRecords = () => {
    statisticsRequest.current?.abort();
    setData(null);
    setRecordsVersion((value) => value + 1);
  };
  return {
    data,
    error,
    loading,
    invalidateRecords,
    refresh: () => setManualRefresh((value) => value + 1),
  };
}
