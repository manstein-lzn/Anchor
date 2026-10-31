/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';
import type { GraphPluginActivation, OurGraph } from './model';
import { graphSaveNotice, useGraphEditor } from './useGraphEditor';

vi.mock('./api', async original => ({ ...await original<object>(), api: vi.fn() }));
const definition: OurGraph = { entry: 'assistant', agents: { assistant: { model: 'models.default.flash' } }, nodes: [{ id: 'assistant', agent: 'assistant' }], edges: [] };
const nextTurn: GraphPluginActivation = { mode: 'next_turn', graph_digest: 'version-1' };

describe('Graph plugin activation metadata', () => {
  let root: Root;
  let container: HTMLDivElement;
  let editor: ReturnType<typeof useGraphEditor>;
  const Harness = ({ name }: { name: string }) => { editor = useGraphEditor(name, [{ graph: name, running: null }], () => {}); return null; };
  const render = async (name = 'g') => { await act(async () => root.render(<Harness name={name} />)); };
  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    container = document.createElement('div'); root = createRoot(container);
    vi.mocked(api).mockReset().mockResolvedValue({ definition, plugin_activation: nextTurn });
  });
  afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

  it('reads server capability and uses the save response for activation feedback', async () => {
    await render(); expect(editor.pluginActivation).toEqual(nextTurn);
    await act(async () => editor.patch({ ...definition, nodes: [{ ...definition.nodes[0], plugins: ['docmost'] }] }));
    expect(editor.dirty).toBe(true);
    const saved: GraphPluginActivation = { mode: 'next_turn', graph_digest: 'version-2' };
    vi.mocked(api).mockResolvedValue({ plugin_activation: saved });
    let activation: GraphPluginActivation | undefined;
    await act(async () => { activation = await editor.save(); });
    expect(editor.dirty).toBe(false); expect(editor.pluginActivation).toEqual(saved);
    expect(graphSaveNotice(activation)).toBe('已保存，插件配置将在各会话的下一条消息生效。');
    expect(vi.mocked(api).mock.calls.at(-1)?.slice(0, 3)).toEqual(['/graphs/g', 'PUT', { definition: editor.doc }]);
  });

  it('keeps rejected changes unsaved and preserves the previous server metadata', async () => {
    await render();
    await act(async () => editor.patch({ ...definition, objective: 'changed structure' }));
    vi.mocked(api).mockRejectedValue(new Error('请先停止并退役现有实例'));
    await act(async () => { await expect(editor.save()).rejects.toThrow('请先停止并退役现有实例'); });
    expect(editor.dirty).toBe(true); expect(editor.pluginActivation).toEqual(nextTurn);
  });

  it('does not infer next-Turn capability from the Graph shape or an old response', async () => {
    await render();
    vi.mocked(api).mockResolvedValue({ definition });
    await render('ordinary');
    expect(editor.pluginActivation).toBeUndefined();
    await act(async () => { expect(graphSaveNotice(await editor.save())).toBe('已保存。'); });
  });

  it('keeps absent and unknown activation modes conservative', () => {
    expect(graphSaveNotice()).toBe('已保存。');
    expect(graphSaveNotice({ mode: 'new_run', graph_digest: 'g' }, true)).toBe('校验通过，工作流已保存。');
    expect(graphSaveNotice({ mode: 'future_mode', graph_digest: 'g' } as unknown as GraphPluginActivation)).toBe('已保存。');
  });

  it('creates roles from the Host default reference and retains explicit legacy aliases', async () => {
    const legacy = { ...definition, agents: { assistant: { model: 'models.academic' } } };
    vi.mocked(api).mockResolvedValueOnce({ definition: legacy });
    await render();
    vi.mocked(api).mockResolvedValueOnce({ execution: { default_reference: 'models.backup.pro' } });
    await act(async () => editor.addAgent());
    expect(vi.mocked(api).mock.calls.at(-1)?.[0]).toBe('/settings/model');
    expect(editor.doc?.agents?.agent).toEqual({ model: 'models.backup.pro', network: false, instructions: '' });
    expect(editor.doc?.agents?.assistant.model).toBe('models.academic');
  });

  it('uses the same Host default when a parallel region needs its first role', async () => {
    vi.mocked(api).mockResolvedValueOnce({ definition: { entry: '', nodes: [], edges: [] } });
    await render();
    vi.mocked(api).mockResolvedValueOnce({ execution: { default_reference: 'models.default.flash' } });
    await act(async () => editor.addParallel());
    expect(editor.doc?.agents?.['parallel-agent']).toEqual({ model: 'models.default.flash', network: false, instructions: '' });
  });

  it('does not overwrite intervening draft changes while reading model settings', async () => {
    await render();
    let resolve: ((value: unknown) => void) | undefined;
    vi.mocked(api).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    let pending: Promise<void>;
    await act(async () => { pending = editor.addAgent(); });
    await act(async () => editor.patch({ ...definition, objective: 'newer draft' }));
    await act(async () => { resolve!({ execution: { default_reference: 'models.default.flash' } }); await pending!; });
    expect(editor.doc?.objective).toBe('newer draft');
    expect(editor.doc?.agents?.agent).toBeUndefined();
  });
});
