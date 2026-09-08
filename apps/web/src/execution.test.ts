import { describe, expect, it } from 'vitest';
import { decisionFor, elapsed, latestNodes, nodeState, executionAttempt, reviewOutcome, reviewDisplay, type ExecutionNode, type ExecutionLease, type ExecutionDecision } from './execution';
const node = (attempt: number, status = 'completed') => ({ id: `n${attempt}`, node_id: 'research', attempt, status, created_at: '2026-09-07T00:00:00Z', updated_at: '2026-09-07T00:00:10Z' } as ExecutionNode);
describe('execution graph projections', () => {
  it('labels attempts as executions and distinguishes retries from completed iterations', () => {
    expect(executionAttempt(node(1), [node(0, 'failed')])).toBe('第 2 次执行（失败后重试）');
    expect(executionAttempt(node(2), [node(1)])).toBe('第 3 次执行');
  });
  it('does not equate preflight completion with independent review approval', () => {
    const outcome = reviewOutcome(JSON.stringify({ verdict: 'revise', review_origin: 'deterministic_preflight', issues: [{ problem: 'Missing citations' }] }));
    expect(reviewDisplay(outcome)).toEqual({ state: 'revise', statusLabel: '预检未通过', detail: '未调用独立学术评审 · Missing citations' });
  });
  it('reads the gate verdict including mechanical issues and blocked outcomes', () => {
    const outcome = reviewOutcome(JSON.stringify({ review: { verdict: 'blocked', mechanical_issues: ['Budget exhausted'] } }));
    expect(reviewDisplay(outcome, true)).toEqual({ state: 'blocked', statusLabel: '需要人工处理', detail: 'Budget exhausted' });
    expect(reviewDisplay(reviewOutcome('{"verdict":"pass"}')).statusLabel).toBe('评审通过');
    expect(reviewDisplay(reviewOutcome('{"review":{"verdict":"pass"}}'), true).statusLabel).toBe('证据检查通过');
  });
  it('shows unavailable conclusions honestly for missing or malformed artifacts', () => {
    for (const content of ['not json', 'null', '{}', '{"verdict":"unknown"}']) {
      expect(reviewOutcome(content)).toBeUndefined();
      expect(reviewDisplay(reviewOutcome(content)).statusLabel).toContain('结论未获取');
    }
  });
  it('selects latest attempts independently of database ordering', () => {
    expect(latestNodes([node(2, 'running'), node(0), node(1)]).get('research')?.attempt).toBe(2);
  });
  it('never shows an old decision as a current-round path', () => {
    const decisions: ExecutionDecision[] = [{ edge_index: 1, source_attempt: 0, source_node_id: 'a', target_node_id: 'b', selected: true, reason: 'condition_true', evaluator: 'jmespath', evaluator_version: '1', decided_at: '2026' }];
    expect(decisionFor(decisions, 1, 1)).toBeUndefined();
    expect(decisionFor(decisions, 1, 0)?.selected).toBe(true);
  });
  it('shows missing or stale heartbeat as interrupted, not healthy work', () => {
    expect(nodeState(node(0, 'running'), [])).toBe('stalled');
    const lease = { state: 'healthy', recoverable: false, reason: 'recent', lease: { node_run_id: 'n0', node_id: 'research', worker_id: 'w', claim_id: 'c', run_id: 'r', acquired_at: '2026', heartbeat_at: '2026' } } as ExecutionLease;
    expect(nodeState(node(0, 'running'), [lease])).toBe('running');
    expect(nodeState(node(0), [])).toBe('completed');
  });
  it('freezes elapsed time for terminal runs', () => {
    expect(elapsed('2026-09-07T00:00:00Z', '2026-09-07T00:01:05Z')).toBe('1:05');
  });
});
