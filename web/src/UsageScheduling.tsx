import { useId } from 'react';
import { ArrowUpRight } from 'lucide-react';
import type { Operations } from './api';

const number = new Intl.NumberFormat('zh-CN');

export function UsageScheduling({ operations }: { operations: Operations }) {
  const headingId = useId();
  const free = Math.max(0, operations.globalLimit - operations.active);
  return (
    <section className="panel usage-scheduling" aria-labelledby={headingId}>
      <div className="section-heading">
        <div>
          <h2 id={headingId}>账户调度</h2>
          <p>实时容量与排队</p>
        </div>
      </div>
      <div className="usage-scheduling-content">
        <div className="usage-scheduling-capacity">
          <span>并发占用</span>
          <strong>
            {number.format(operations.active)}{' '}
            <span>/ {number.format(operations.globalLimit)}</span>
          </strong>
          <progress
            max={Math.max(1, operations.globalLimit)}
            value={Math.min(operations.active, Math.max(1, operations.globalLimit))}
            aria-label="全局并发占用"
            aria-valuetext={`${operations.active} / ${operations.globalLimit}`}
          />
        </div>
        <dl className="usage-scheduling-counts">
          <div>
            <dt>并发上限</dt>
            <dd>{number.format(operations.globalLimit)}</dd>
          </div>
          <div>
            <dt>空闲并发</dt>
            <dd>{number.format(free)}</dd>
          </div>
          <div>
            <dt>正在排队</dt>
            <dd>{number.format(operations.queued)}</dd>
          </div>
        </dl>
        <dl className="usage-scheduling-mode">
          <div>
            <dt>上限模式</dt>
            <dd>{operations.globalLimitAuto ? '随账户容量调整' : '手动设置'}</dd>
          </div>
          <div>
            <dt>账户总容量</dt>
            <dd>{number.format(operations.accountCapacity)}</dd>
          </div>
        </dl>
        <p className="usage-scheduling-note">
          全局实时值，不受日期与筛选条件影响。空闲并发不代表当前分组一定有可用账户。
        </p>
      </div>
      <a className="usage-scheduling-link" href="?view=accounts">
        管理账户与并发 <ArrowUpRight size={14} aria-hidden="true" />
      </a>
    </section>
  );
}
