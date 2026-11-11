import { useId } from 'react';
import type { NetworkMode } from './model';

/** Policy is controlled by the fact owner; effective access never substitutes for a saved mode. */
export function NetworkPolicy({ mode, onChange, disabled = false, pending = false, scope, effective, enabledBy = [] }: {
  mode?: NetworkMode; onChange: (mode: NetworkMode) => void; disabled?: boolean; pending?: boolean;
  scope: 'node' | 'session'; effective?: boolean; enabledBy?: string[];
}) {
  const id = useId();
  const context = scope === 'node' ? '此节点' : '此会话';
  return <div className="plugin-network-policy" aria-busy={pending}>
    <label htmlFor={id}>联网策略</label>
    <select id={id} value={mode ?? ''} disabled={disabled || pending || mode === undefined}
      aria-describedby={`${id}-description`} onChange={event => onChange(event.target.value as NetworkMode)}>
      {mode === undefined && <option value="">联网策略未知</option>}
      <option value="auto">自动（按插件依赖）</option>
      <option value="enabled">开启联网</option>
      <option value="denied">关闭（禁止联网）</option>
    </select>
    <p id={`${id}-description`}>{mode === undefined ? '无法确认已保存的联网策略，请刷新后重试。'
      : mode === 'denied' ? `${context}的所有任务工具均禁止联网，插件依赖不能重新启用。`
        : mode === 'enabled' ? '主动启用联网，仍受宿主授权约束。'
          : '仅按插件依赖启用；没有联网需求时不开放网络。'}
      {mode !== 'denied' && enabledBy.length > 0 && ` 由${enabledBy.join('、')}依赖启用。`}
      {effective !== undefined && ` 当前网络访问${effective ? '已允许' : '未启用'}。`}
    </p>
  </div>;
}
