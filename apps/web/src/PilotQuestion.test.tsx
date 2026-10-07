import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { PilotQuestion } from './PilotQuestion';
import type { Question } from './pilotQuestions';

const pending: Question = {
  id: 'q-1', session: 's-1', turn: 't-1', message: '确认删除工作流？',
  requested_schema: { type: 'object', properties: { confirm: { type: 'boolean', title: '确认删除', default: true } }, required: ['confirm'] },
  status: 'pending', answer: null,
};
const render = (question: Question, enabled = true, stopping = false) => renderToStaticMarkup(
  <PilotQuestion question={question} enabled={enabled} stopping={stopping} onAnswered={() => undefined} />,
);
describe('Pilot question presentation', () => {
  it('shows a primitive form with explicit confirmation selection and all three actions', () => {
    const html = render(pending);
    expect(html).toContain('确认删除工作流？');
    expect(html).toContain('<select');
    expect(html).toContain('<option value="" selected="">请选择</option>');
    expect(html).toContain('<option value="true">是</option>');
    expect(html).toContain('<option value="false">否</option>');
    expect(html).toContain('>回答</button>');
    expect(html).toContain('>拒绝</button>');
    expect(html).toContain('>取消</button>');
    expect(html).not.toContain('textarea');
  });
  it('renders string, integer, number and enum controls with accessible labels', () => {
    const html = render({ ...pending, requested_schema: { type: 'object', properties: {
      name: { type: 'string', title: '名称', description: '用于识别工作流' },
      count: { type: 'integer' }, rate: { type: 'number' }, mode: { type: 'string', enum: ['fast', 'slow'] },
    } } });
    expect(html).toContain('type="text"');
    expect(html).toContain('step="1"');
    expect(html).toContain('step="any"');
    expect(html).toContain('aria-describedby="question-q-1-name-description"');
    expect(html).toContain('>fast</option>');
  });
  it('explicitly rejects unsupported schemas while leaving decline/cancel available', () => {
    const html = render({ ...pending, requested_schema: { type: 'object', properties: { nested: { type: 'object' } } } });
    expect(html).toContain('暂不支持');
    expect(html).toMatch(/<button[^>]+disabled=""[^>]*>回答<\/button>/);
    expect(html).toContain('>拒绝</button>');
    expect(html).toContain('>取消</button>');
    expect(html).not.toContain('<input');
    expect(html).not.toContain('<textarea');
  });
  it.each([false, true])('removes answer controls when execution is stopped or stopping (%s)', stopping => {
    const html = render(pending, false, stopping);
    expect(html).not.toContain('<form');
    expect(html).not.toContain('<select');
    expect(html).toContain(stopping ? '正在停止' : '执行已结束');
  });
  it.each(['accept', 'decline', 'cancel'] as const)('keeps saved %s history visible without answer controls', action => {
    const html = render({ ...pending, status: 'answered', answer: { action, ...(action === 'accept' ? { content: { confirm: false } } : {}) } });
    expect(html).toContain({ accept: '已回答', decline: '已拒绝', cancel: '已取消' }[action]);
    expect(html).not.toContain('<form');
    if (action === 'accept') expect(html).toContain('<dd>否</dd>');
  });
  it('keeps interrupted question history visible without answer controls', () => {
    const html = render({ ...pending, status: 'interrupted' });
    expect(html).toContain('问题已中断');
    expect(html).not.toContain('<form');
  });
  it('does not invent a cancellation when answered history has no saved response', () => {
    const html = render({ ...pending, status: 'answered', answer: null });
    expect(html).toContain('已回答');
    expect(html).not.toContain('已取消');
  });
});
