import { describe, expect, it } from 'vitest';
import { intervalFacts, schedulePlanNote, timelineEntries, timelineNeedsAttention } from './Timeline';
import { assignGraphColors } from './graphColors';
import type { OurRun } from './model';

const dateText = (value: string | number) => new Date(value).toLocaleString('zh-CN', { month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit' });

describe('timeline Graph colors', () => {
  it('assigns a deterministic, perceptually spread palette to the current Graph set', () => {
    const names = ['deep-academic-research', 'rsi', 'wecom-assistant', 'weekly-work-report'];
    const colors = assignGraphColors(names);
    expect(colors).toEqual(assignGraphColors([...names].reverse()));
    expect(new Set(Object.values(colors)).size).toBe(names.length);
    expect(colors.rsi).not.toBe(colors['deep-academic-research']);
    const hue = (color: string) => Number(color.match(/^hsl\((\d+\.\d+)/)?.[1]);
    expect(Math.abs(hue(colors.rsi) - hue(colors['deep-academic-research']))).toBeGreaterThan(30);
    expect(assignGraphColors([])).toEqual({});
    expect(assignGraphColors(['same', 'same'])).toEqual(assignGraphColors(['same']));
  });
});

const run = (overrides: Partial<OurRun> = {}): OurRun => ({
  run: 'assistant-1', graph: 'wecom-persistent-assistant', running: true,
  projection: { phase: 'executing', active: true, reason: null, updated: null, actions: { pause: false, stop: true, resume: false, abandon: true } },
  started: '2026-09-28T09:00:00', updated: '2026-09-28T09:00:00', executed: [], objective: '常驻助手',
  ...overrides,
});

describe('resident Run bars', () => {
  const now = +new Date('2026-09-30T12:00:00');

  it('draws the execution windows of a resident Run instead of its whole lifetime', () => {
    const entries = timelineEntries(run({ activity: [
      { start: '2026-09-28T09:00:00', end: '2026-09-28T09:05:00' },
      { start: '2026-09-30T11:00:00', end: '2026-09-30T11:00:00', running: true },
    ] }), now);
    expect(entries.map(entry => [entry.start, entry.end])).toEqual([
      [+new Date('2026-09-28T09:00:00'), +new Date('2026-09-28T09:05:00')],
      [+new Date('2026-09-30T11:00:00'), now],
    ]);
    expect(entries.map(entry => entry.open)).toEqual([false, true]);
    expect(entries.every(entry => entry.run?.run === 'assistant-1')).toBe(true);
    expect(new Set(entries.map(entry => entry.id)).size).toBe(2);
  });

  it('leaves a completed window closed even though the resident Run is still alive', () => {
    const [entry] = timelineEntries(run({ activity: [
      { start: '2026-09-29T08:00:00', end: '2026-09-29T08:30:00' },
    ] }), now);
    expect(entry.end).toBe(+new Date('2026-09-29T08:30:00'));
    expect(entry.end).toBeLessThan(now);
  });

  it('marks an instance that reports no window yet instead of stretching it to now', () => {
    const [entry] = timelineEntries(run({ activity: [], updated: '2026-09-28T09:00:30' }), now);
    expect(entry).toEqual(expect.objectContaining({ id: 'assistant-1', resident: true, start: +new Date('2026-09-28T09:00:00'), end: +new Date('2026-09-28T09:00:00') }));
    expect(entry.open).toBeUndefined();
    const [broken] = timelineEntries(run({ activity: [{ start: 'not-a-date', end: '2026-09-29T08:30:00' }] }), now);
    expect(broken.end).toBe(broken.start);
  });

  it('keeps one growing bar for a Run without an activity projection', () => {
    expect(timelineEntries(run(), now)).toEqual([expect.objectContaining({
      id: 'assistant-1', start: +new Date('2026-09-28T09:00:00'), end: now, open: true,
    })]);
    const [finished] = timelineEntries(run({ running: false, projection: { phase: 'settled', outcome: 'done', active: false, reason: null, updated: null, actions: { pause: false, stop: false, resume: false, abandon: false } }, updated: '2026-09-29T10:30:00' }), now);
    expect(finished.end).toBe(+new Date('2026-09-29T10:30:00'));
    expect(finished.open).toBe(false);
  });
});

describe('bar interval facts', () => {
  const now = +new Date('2026-09-30T12:00:00');
  const factsFor = (overrides: Partial<OurRun> = {}, entryOverrides: Record<string, unknown> = {}) => {
    const [entry] = timelineEntries(run(overrides), now);
    return intervalFacts({ ...run(overrides) }, { ...entry, ...entryOverrides }, now);
  };

  it('reports a still-running interval as executing now', () => {
    expect(factsFor().label).toBe('已运行');
  });

  it('keeps the restart message for a Run the host no longer owns', () => {
    expect(factsFor({ running: false, updated: '2026-09-29T10:30:00', projection: {
      phase: 'executing', active: false, reason: null, updated: null,
      actions: { pause: false, stop: true, resume: true, abandon: true },
    } })).toEqual({
      label: '运行状态', value: '宿主重启后等待接续',
    });
  });

  it('reports a resident segment by its own window, not the Run lifetime', () => {
    expect(factsFor({ activity: [{ start: '2026-09-29T08:00:00', end: '2026-09-29T08:30:00' }] })).toEqual({
      label: '这段时长', value: '30 分钟', end: dateText(+new Date('2026-09-29T08:30:00')),
    });
    expect(factsFor({ activity: [] }, { resident: true, end: +new Date('2026-09-28T09:00:00') })).toEqual({
      label: '这段时长', value: '暂无记录', end: '等待下一轮输入',
    });
    // A stopped instance is not waiting for anything.
    expect(factsFor({ running: false, activity: [], projection: { phase: 'waiting', waiting_for: 'human', active: false, reason: 'stopped', updated: null, actions: { pause: false, stop: false, resume: true, abandon: true } } }, { resident: true, end: +new Date('2026-09-28T09:00:00') })).toEqual({
      label: '这段时长', value: '暂无记录', end: '暂无记录',
    });
  });
});

describe('shared Run status and attention projection', () => {
  it('includes waiting recovery in the same attention filter used by the Timeline', () => {
    const recovering = run({ running: false, projection: {
      phase: 'waiting', waiting_for: 'effect', active: false, reason: '工具结果未记录', updated: null,
      actions: { pause: false, stop: false, resume: false, abandon: true },
    } });
    const [entry] = timelineEntries(recovering, Date.now());
    expect(entry.status).toBe('waiting');
    expect(timelineNeedsAttention(entry)).toBe(true);
    recovering.projection = { phase: 'settled', outcome: 'done', active: false, reason: null, updated: null, actions: { pause: false, stop: false, resume: false, abandon: false } };
    expect(timelineNeedsAttention(entry)).toBe(false);
    expect(timelineNeedsAttention({ status: 'missed_busy' })).toBe(true);
    expect(timelineNeedsAttention({ status: 'pending' })).toBe(true);
    expect(timelineNeedsAttention({ status: 'pending_busy' })).toBe(true);
    expect(schedulePlanNote('pending')).toContain('不会因延迟而丢弃');
    expect(schedulePlanNote('pending_busy')).toContain('空闲后继续尝试启动');
    expect(schedulePlanNote('missed_busy')).toContain('历史记录');
  });
});
