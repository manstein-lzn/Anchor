import { label } from './execution';
import type { OurRunState, RunProjection } from './model';
import { activeNodes } from './parallel';

export type RunControl = 'pause' | 'stop' | 'resume' | 'abandon';
export type ProjectedRun = { projection?: RunProjection | null };

/** All entry points consume the Host's actions; a missing projection grants nothing. */
export const runPhase = (run: ProjectedRun) => run.projection?.phase ?? 'unavailable';
export const runNeedsAttention = ({ projection: p }: ProjectedRun) => !p || (!p.superseded && (p.phase === 'waiting'
  || (p.phase === 'executing' && !p.active) || (p.phase === 'settled' && p.outcome !== 'done')));
export const runQuiescent = (p?: RunProjection) => !!p && !p.active
  && (p.phase === 'settled' || (p.phase === 'waiting' && p.waiting_for === 'human'));
const supersededLabel = '已由新实例接替';

export function runPhaseLabel(value?: RunProjection | string | null): string {
  if (!value) return '运行状态不可用';
  if (typeof value === 'string') return ({ executing: '执行中', waiting: '等待处理', settled: '已结算',
    unavailable: '运行状态不可用', planned: '已计划', pending: '等待启动', pending_busy: '忙碌待启动', missed_busy: '忙碌错过', missed_downtime: '停机错过',
  }[value] ?? label(value));
  if (value.superseded && !value.active && value.outcome !== 'done') {
    if (value.phase !== 'settled') return supersededLabel;
    return `${runPhaseLabel({ ...value, superseded: false })}，${supersededLabel}`;
  }
  if (value.phase === 'executing') return value.active ? '执行中' : '等待接续';
  if (value.phase === 'waiting') {
    const who = { human: '你处理', child: '子任务', effect: '核查效果' }[value.waiting_for ?? 'human'];
    return `等待${who}${value.reason ? `：${value.reason}` : ''}`;
  }
  return ({ done: '已完成', failed: '失败', escalated: '已升级处理', unknown: '结果未知', aborted: '已放弃' }[value.outcome ?? 'unknown']);
}

export const runControlLabels: Record<RunControl, string> = { pause: '暂停', stop: '停止', resume: '继续', abandon: '放弃' };

/** A cursor locates retained progress; only an active executing observation is live. */
export function runNodePresentation(state: OurRunState | null | undefined, node: string, projection?: RunProjection | null) {
  const atCursor = activeNodes(state).some(cursor => cursor.node === node);
  const phase = runPhase({ projection });
  const live = atCursor && projection?.active === true && phase === 'executing';
  if (atCursor) {
    const text = runPhaseLabel(projection);
    return { status: phase, label: text, summaryLabel: text, atCursor, live };
  }
  const result = state?.nodes?.[node];
  if (result) {
    const status = result.exit_status === 'interrupted' ? 'interrupted' : result.submitted ? 'completed' : 'failed';
    const text = label(result.exit_status || status);
    return { status, label: text, summaryLabel: status === 'completed' ? '已提交' : text, atCursor, live };
  }
  if (state?.skipped?.includes(node)) return { status: 'skipped', label: '未选中', summaryLabel: '未选中', atCursor, live };
  if (state !== null && !projection) return { status: 'unavailable', label: runPhaseLabel('unavailable'), summaryLabel: runPhaseLabel('unavailable'), atCursor, live };
  return { status: 'pending', label: '尚未执行', summaryLabel: '未执行', atCursor, live };
}
