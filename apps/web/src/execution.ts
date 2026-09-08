export type ExecutionRun = { id: string; task_id: string; graph_version_id: string; status: string; current_phase: string; revision: number; last_event_sequence: number; workflow_version: string; created_at: string; updated_at: string };
export type ExecutionNode = { id: string; node_id: string; attempt: number; revision: number; status: string; output_ref?: string | null; error_code?: string | null; input_hash?: string | null; context_generation: number; created_at: string; updated_at: string };
export type ExecutionLease = { state: string; recoverable: boolean; reason: string; node_type?: string; lease: { node_run_id: string; node_id: string; worker_id: string; claim_id: string; run_id: string; acquired_at: string; heartbeat_at: string } };
export type ExecutionDecision = { edge_index: number; source_attempt: number; source_node_id: string; target_node_id: string; selected: boolean; reason: string; condition?: string | null; evaluator: string; evaluator_version: string; evaluation_context_hash?: string | null; evidence_ref?: string | null; decided_at: string };
export type ExecutionOperation = { operation_id: string; node_run_id: string; tool_ref: string; status: string; request_hash: string; updated_at: string; result_ref?: string | null; error_code?: string | null; reconciliation_ref?: string | null };

export const executionLabels: Record<string, string> = { created: '已接纳', queued: '已排队', pending: '等待依赖', ready: '等待执行', running: '执行中', completed: '已完成', failed: '失败', cancelled: '已取消', skipped: '已跳过', waiting_approval: '等待人工处理', waiting_event: '等待事件', stalled: '心跳中断', retrying: '等待重试', paused: '已暂停', succeeded: '成功', registered: '已登记', outcome_unknown: '结果未知', execution_budget_exceeded: '总预算耗尽', agent_timeout: '模型节点超时', agent_output_invalid: '模型输出无效', agent_execution_failed: '上游模型失败', control_input_invalid: '执行输入无效', source_rate_limited: '来源限流', source_timeout: '来源超时', source_unavailable: '来源暂不可用', source_access_denied: '来源拒绝访问', source_not_found: '来源不存在', source_http_error: '来源请求失败', source_too_large: '文档过大', retrieval_failed: '检索处理失败', tool_budget_exceeded: '工具预算耗尽', passed: '通过', rejected: '拒绝', error: '错误', blocked: '已阻塞' };
export const label = (status: string) => executionLabels[status] ?? status;
export const terminal = (status: string) => ['completed', 'failed', 'cancelled'].includes(status);
export type ReviewOutcome = { verdict: 'pass' | 'revise' | 'blocked'; preflight: boolean; issues: string[] };

export function reviewOutcome(content: string): ReviewOutcome | undefined {
  try {
    const output = JSON.parse(content);
    const review = output?.review ?? output;
    if (!['pass', 'revise', 'blocked'].includes(review?.verdict)) return undefined;
    return { verdict: review.verdict, preflight: review.review_origin === 'deterministic_preflight',
      issues: [...new Set([...(Array.isArray(review.issues) ? review.issues.map((issue: { problem?: string }) => issue?.problem) : []),
        ...(Array.isArray(review.mechanical_issues) ? review.mechanical_issues : [])].filter((issue): issue is string => typeof issue === 'string'))] };
  } catch { return undefined; }
}

export function reviewDisplay(outcome: ReviewOutcome | undefined, gate = false) {
  if (!outcome) return { state: 'pending', statusLabel: '检查完成 · 结论未获取', detail: '结论暂不可用' };
  const statusLabel = outcome.verdict === 'blocked' ? '需要人工处理'
    : outcome.verdict === 'revise' ? (outcome.preflight && !gate ? '预检未通过' : '需要修订')
    : gate ? '证据检查通过' : '评审通过';
  return { state: outcome.verdict === 'pass' ? 'completed' : outcome.verdict, statusLabel,
    detail: [outcome.preflight ? '未调用独立学术评审' : '', ...outcome.issues].filter(Boolean).join(' · ') };
}

export function executionAttempt(node: ExecutionNode, history: ExecutionNode[]): string {
  const prior = history.find(item => item.node_id === node.node_id && item.attempt === node.attempt - 1);
  return `第 ${node.attempt + 1} 次执行${prior?.status === 'failed' ? '（失败后重试）' : ''}`;
}
export function latestNodes(nodes: ExecutionNode[]): Map<string, ExecutionNode> {
  const result = new Map<string, ExecutionNode>();
  for (const node of nodes) if (!result.has(node.node_id) || result.get(node.node_id)!.attempt < node.attempt) result.set(node.node_id, node);
  return result;
}
export function nodeState(node: ExecutionNode | undefined, leases: ExecutionLease[]): string {
  if (!node) return 'pending';
  const lease = leases.find(item => item.lease.node_run_id === node.id);
  return node.status === 'running' && (!lease || lease.state !== 'healthy') ? 'stalled' : node.status;
}
export function elapsed(start: string, end: string | number = Date.now()): string {
  const seconds = Math.max(0, Math.floor((Number(new Date(end)) - Number(new Date(start))) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}
export function decisionFor(decisions: ExecutionDecision[], edge: number, attempt: number | undefined) {
  return decisions.filter(item => item.edge_index === edge && item.source_attempt === attempt)
    .sort((a, b) => b.decided_at.localeCompare(a.decided_at))[0];
}

export type ExecutionTask = { id: string; objective: string; constraints: string[]; success_criteria: string[]; status: string };
export type ExecutionEvent = { sequence: number; event_type: string; payload: Record<string, unknown>; created_at: string };
export type ExecutionContext = { id: string; node_run_id: string; generation: number; input_hash: string; snapshot: Record<string, unknown>; created_at: string };
export type ExecutionVerification = { verification_id: string; node_id: string; verifier_ref: string; verifier_version: string; adapter: string; adapter_version: string; verdict: string; reason: string; evidence_ref: string; verified_artifact_hashes: string[]; verified_context_hash: string; decided_at: string };
export type ExecutionMemory = { memory_id: string; content: string; content_hash: string; created_at: string; deleted_at?: string | null };
export type ExecutionWait = { node_run: { id: string; node_id: string; status: string }; run_id: string; node_type: string };
export type ExecutionDiagnostic = { diagnostic_id: string; run_id: string; node_run_id?: string | null; reason: string; evidence_refs: string[]; suggested_actions: string[]; created_at: string; status: string };
export type ExecutionProgress = { evidence_id: string; state_revision: number; phase: string; artifact_refs: string[]; verifier_passes: number; tool_operation_ids: string[]; verified_progress_refs: string[]; cycle_iteration: number; cycle_fingerprint?: string | null; waiting_for?: string | null; heartbeat_at: string };

export const tone = (status: string) => ['failed', 'outcome_unknown', 'rejected', 'error', 'blocked'].includes(status) ? 'danger'
  : ['completed', 'succeeded', 'passed'].includes(status) ? 'success'
  : ['running', 'ready', 'retrying'].includes(status) ? 'active' : 'neutral';
