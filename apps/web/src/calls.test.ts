import { describe, expect, it } from 'vitest';
import { previewCallInput, validateCallTargets } from './calls';
import type { OurGraph } from './model';

describe('structured call input', () => {
  it('selects only explicit parameters and decodes JSON pointers', () => {
    expect(previewCallInput({ graph: 'target', mode: 'wait', input: { fixed: 1 }, input_map: { text: '/a~1b/~0value', first: '/items/0' } },
      { 'a/b': { '~value': 'report' }, items: [42], secret: 'not forwarded' })).toEqual({ fixed: 1, text: 'report', first: 42 });
  });
  it('reports missing and invalid pointers without evaluating input', () => {
    expect(() => previewCallInput({ graph: 'target', mode: 'wait', input_map: { a: '/missing' } })).toThrow('运行时必须提供');
    expect(() => previewCallInput({ graph: 'target', mode: 'wait', input_map: { a: 'process.env' } })).toThrow('有效的 JSON Pointer');
    expect(() => previewCallInput({ graph: 'target', mode: 'wait', input_map: { a: '/bad~2' } })).toThrow('有效的 JSON Pointer');
  });
  it('rejects a broken installed target reference before saving', () => {
    const graph: OurGraph = { entry: 'call', nodes: [{ id: 'call', op: 'call' }], edges: [], ops: { call: { call: { graph: 'missing', mode: 'detach' } } } };
    expect(() => validateCallTargets(graph, ['installed'])).toThrow('目标工作流不存在');
  });
});
