/** @vitest-environment jsdom */
import { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphTaskPolicyEditor } from './GraphTaskPolicyEditor';
import type { GraphTaskPolicy } from './model';

describe('Graph task authorization editor', () => {
  let root: Root;
  let container: HTMLDivElement;
  let policy: GraphTaskPolicy;
  const catalog = [
    { id: 'search', name: 'Search', description: 'Research sources' },
    { id: 'graph-tasks', name: 'Graph Tasks', description: 'Delegation' },
  ];
  function Harness({ initial = {}, disabled = false }: { initial?: GraphTaskPolicy; disabled?: boolean }) {
    const [value, setValue] = useState(initial); policy = value;
    return <GraphTaskPolicyEditor value={value} onChange={setValue} graphs={['research', 'briefing']}
      catalog={catalog} catalogKnown disabled={disabled} onDetails={() => {}} />;
  }
  const checkbox = (text: string) => [...container.querySelectorAll('label')].find(label => label.textContent?.includes(text))!
    .querySelector('input[type="checkbox"]') as HTMLInputElement;
  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    container = document.createElement('div'); root = createRoot(container);
  });
  afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

  it('starts closed, scopes Graph discovery, and grants candidate testing independently of creation', async () => {
    await act(async () => root.render(<Harness />));
    expect([...container.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')].every(input => !input.checked)).toBe(true);
    expect(container.querySelector('[role="switch"][aria-checked="true"]')).toBeNull();
    await act(async () => { checkbox('research').click(); checkbox('允许准备和测试候选插件').click(); });
    expect(policy).toEqual({ graphs: ['research'], develop_plugins: true });
    expect(checkbox('允许构建新的工作流').checked).toBe(false);
    expect(container.querySelector('[aria-label="授权新建 Graph 使用 Graph Tasks"]')).toBeNull();
    await act(async () => (container.querySelector('[aria-label="授权新建 Graph 使用 Search"]') as HTMLButtonElement).click());
    expect(policy.plugins).toEqual(['search']);
    expect(container.textContent).toContain('新建 Graph 能力上限');
    expect(container.textContent).toContain('允许新建的子任务联网（不修改当前助手）');
  });

  it('keeps unavailable choices visible for revocation and respects disabled editing', async () => {
    const initial = { graphs: ['removed-graph'], plugins: ['removed-plugin'], network: true };
    await act(async () => root.render(<Harness initial={initial} />));
    expect(checkbox('removed-graph').checked).toBe(true);
    expect(container.textContent).toContain('当前列表中不可用，可取消授权');
    await act(async () => (container.querySelector('[aria-label="取消新建 Graph 授权 removed-plugin"]') as HTMLButtonElement).click());
    expect(policy.plugins).toEqual([]);
    expect(policy.graphs).toEqual(['removed-graph']);
    await act(async () => root.render(<Harness disabled />));
    expect(container.querySelector('fieldset')?.disabled).toBe(true);
    expect((container.querySelector('[role="switch"]') as HTMLButtonElement).disabled).toBe(true);
  });
});
