import { afterEach, describe, expect, it, vi } from 'vitest';
import * as transport from './api';
import { answerQuestion, loadQuestionHistory, mergeQuestions, questionContent, questionFields, questionsPath, type Question } from './pilotQuestions';

const question: Question = {
  id: 'q-1', session: 's-1', turn: 't-1', message: '确认删除这个工作流？',
  requested_schema: { type: 'object', properties: { confirm: { type: 'boolean', title: '确认删除', default: true } }, required: ['confirm'] },
  status: 'pending', answer: null,
};
describe('question form schema', () => {
  it('supports flat primitive fields, constraints and typed enum values', () => {
    const form = questionFields({ type: 'object', properties: {
      name: { type: 'string', title: '名称', description: '输入名称', minLength: 2, maxLength: 10, pattern: '^[a-z]+$' },
      count: { type: 'integer', minimum: 1, maximum: 5 },
      rate: { type: 'number', multipleOf: 0.1 },
      choice: { type: 'number', enum: [0, 1.5] },
      confirm: { type: 'boolean' },
    }, required: ['name', 'confirm'], additionalProperties: false });
    expect(form.error).toBeUndefined();
    expect(form.fields?.[0]).toMatchObject({ name: 'name', title: '名称', required: true, minLength: 2, maxLength: 10 });
    expect(questionContent(form.fields!, { name: '  keep spaces  ', count: '3', rate: '0.4', choice: '1', confirm: 'false' }))
      .toEqual({ name: '  keep spaces  ', count: 3, rate: 0.4, choice: 1.5, confirm: false });
  });
  it.each([
    true, { type: 'array' }, { type: 'object', properties: { items: { type: 'array' } } },
    { type: 'object', properties: { nested: { type: 'object' } } },
    { type: 'object', properties: { text: { type: ['string', 'null'] } } },
    { type: 'object', oneOf: [] }, { type: 'object', properties: { text: { type: 'string', anyOf: [] } } },
    { type: 'object', properties: { text: { $ref: '#/$defs/text' } } },
    { type: 'object', additionalProperties: { type: 'string' } },
    { type: 'object', properties: { choice: { type: 'string', enum: [false] } } },
    { type: 'object', properties: { choice: { type: 'number', enum: [] } } },
    { type: 'object', required: ['absent'] }, { type: 'object', properties: null }, { type: 'object', required: null },
  ])('fails explicitly for unsupported schema %j', schema => {
    expect(questionFields(schema)).toEqual({ error: expect.stringContaining('暂不支持') });
  });
  it('never implicitly accepts a true confirmation, even when the schema has that default', () => {
    const fields = questionFields(question.requested_schema).fields!;
    expect(() => questionContent(fields, {})).toThrow('确认删除');
    expect(questionContent(fields, { confirm: 'false' })).toEqual({ confirm: false });
    expect(questionContent(fields, { confirm: 'true' })).toEqual({ confirm: true });
    expect(() => questionContent(fields, { confirm: 'yes' })).toThrow('是或否');
  });
  it('preserves primitive enum types and omits optional unanswered fields', () => {
    const fields = questionFields({ type: 'object', properties: {
      confirm: { type: 'boolean', enum: [false, true] },
      text: { type: 'string', enum: ['', 'selected'] },
      optional: { type: 'number' },
    } }).fields!;
    expect(questionContent(fields, { confirm: '0', text: '0' })).toEqual({ confirm: false, text: '' });
    expect(() => questionContent(fields, { confirm: '2' })).toThrow('选项');
  });
  it.each(['NaN', 'Infinity', '  ', '1.5', '9007199254740993'])('does not silently convert an invalid integer %s', value => {
    const fields = questionFields({ type: 'object', properties: { count: { type: 'integer' } } }).fields!;
    expect(() => questionContent(fields, { count: value })).toThrow('整数');
  });
  it('keeps constraints for the backend instead of changing submitted values', () => {
    const fields = questionFields({ type: 'object', properties: { count: { type: 'integer', minimum: 10 } } }).fields!;
    expect(questionContent(fields, { count: '2' })).toEqual({ count: 2 });
  });
  it('retains explicitly entered empty strings for backend validation', () => {
    const fields = questionFields({ type: 'object', properties: { name: { type: 'string' } }, required: ['name'] }).fields!;
    expect(questionContent(fields, { name: '' })).toEqual({ name: '' });
  });
  it('does not mistake object prototype names for user-provided values', () => {
    const fields = questionFields({ type: 'object', properties: { constructor: { type: 'string' } }, required: ['constructor'] }).fields!;
    expect(() => questionContent(fields, {})).toThrow('constructor');
    expect(questionContent(fields, { constructor: 'chosen' })).toEqual({ constructor: 'chosen' });
  });
});
describe('same-turn answer request', () => {
  afterEach(() => vi.restoreAllMocks());
  it.each(['accept', 'decline', 'cancel'] as const)('sends %s only to the existing question endpoint', async action => {
    const answer = { action, ...(action === 'accept' ? { content: { confirm: false } } : {}) };
    const returned: Question = { ...question, status: 'answered', answer };
    const request = vi.spyOn(transport, 'api').mockResolvedValue({ question: returned });
    const controller = new AbortController();
    await expect(answerQuestion(question, answer, controller.signal)).resolves.toEqual(returned);
    expect(request).toHaveBeenCalledTimes(1);
    expect(request).toHaveBeenCalledWith('/sessions/s-1/turns/t-1/questions/q-1/answer', 'POST', answer, controller.signal);
    if (action !== 'accept') expect(request.mock.calls[0][2]).not.toHaveProperty('content');
  });
  it('encodes the question identifier', async () => {
    const scoped = { ...question, id: 'question/one' };
    const request = vi.spyOn(transport, 'api').mockResolvedValue({ question: scoped });
    const controller = new AbortController();
    await answerQuestion(scoped, { action: 'cancel' }, controller.signal);
    expect(request).toHaveBeenCalledWith('/sessions/s-1/turns/t-1/questions/question%2Fone/answer', 'POST', { action: 'cancel' }, controller.signal);
  });
  it('passes backend validation failures through without changing the supplied answer', async () => {
    vi.spyOn(transport, 'api').mockRejectedValue(new Error('confirm is required'));
    const answer = { action: 'accept' as const, content: {} };
    await expect(answerQuestion(question, answer, new AbortController().signal)).rejects.toThrow('confirm is required');
    expect(answer).toEqual({ action: 'accept', content: {} });
  });
  it.each(['id', 'session', 'turn'] as const)('rejects a response for another %s', async key => {
    vi.spyOn(transport, 'api').mockResolvedValue({ question: { ...question, [key]: 'other' } });
    await expect(answerQuestion(question, { action: 'decline' }, new AbortController().signal)).rejects.toThrow('不同的问题');
  });
});
describe('saved question history', () => {
  afterEach(() => vi.restoreAllMocks());
  it('loads past turns in order and rejects foreign questions', async () => {
    const first: Question = { ...question, status: 'answered', answer: { action: 'decline' } };
    const second: Question = { ...question, id: 'q-2', turn: 't-2', status: 'interrupted' };
    const request = vi.spyOn(transport, 'api').mockResolvedValueOnce({ questions: [first, { ...first, session: 'other' }] })
      .mockResolvedValueOnce({ questions: [second, { ...second, turn: 'wrong' }] });
    const controller = new AbortController();
    await expect(loadQuestionHistory('s-1', ['t-1', 't-2'], controller.signal)).resolves.toEqual([first, second]);
    expect(request).toHaveBeenNthCalledWith(1, '/sessions/s-1/turns/t-1/questions', 'GET', undefined, controller.signal);
    expect(request).toHaveBeenNthCalledWith(2, '/sessions/s-1/turns/t-2/questions', 'GET', undefined, controller.signal);
  });
  it('does not replace a failed history request with invented empty history', async () => {
    vi.spyOn(transport, 'api').mockRejectedValue(new Error('history unavailable'));
    await expect(loadQuestionHistory('s-1', ['t-1'], new AbortController().signal)).rejects.toThrow('history unavailable');
  });
});
describe('question history projection', () => {
  it('does not resurrect answered or interrupted questions from a late GET snapshot', () => {
    for (const status of ['answered', 'interrupted'] as const) {
      const final = { ...question, status, answer: status === 'answered' ? { action: 'decline' as const } : null };
      expect(mergeQuestions([final], [question], 's-1', 't-1')).toEqual([final]);
    }
  });
  it('accepts answer/interruption events without changing turn identity', () => {
    const final: Question = { ...question, status: 'answered', answer: { action: 'accept', content: { confirm: true } } };
    expect(mergeQuestions([question], [final], 's-1', 't-1')).toEqual([final]);
    expect(mergeQuestions([question], [{ ...question, status: 'interrupted' }], 's-1', 't-1')[0].status).toBe('interrupted');
  });
  it('rejects late responses from another session or turn', () => {
    expect(mergeQuestions([question], [{ ...question, session: 'other' }, { ...question, turn: 'old' }], 's-1', 't-1')).toEqual([question]);
  });
  it('retains saved history when another question is pending', () => {
    const answered: Question = { ...question, status: 'answered', answer: { action: 'cancel' } };
    expect(mergeQuestions([answered], [{ ...question, id: 'q-2' }], 's-1', 't-1')).toEqual([answered, { ...question, id: 'q-2' }]);
  });
  it('encodes session and turn IDs in the dedicated question endpoint', () => {
    expect(questionsPath('s/one', 't#two')).toBe('/sessions/s%2Fone/turns/t%23two/questions');
  });
});
