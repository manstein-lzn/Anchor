import { api } from './api';

export type QuestionAnswer = { action: 'accept' | 'decline' | 'cancel'; content?: Record<string, unknown> };
export type Question = {
  id: string; session: string; turn: string; message: string; requested_schema: unknown;
  status: 'pending' | 'answered' | 'interrupted'; answer: QuestionAnswer | null;
};
type FieldType = 'string' | 'boolean' | 'integer' | 'number';
type FieldValue = string | boolean | number;
export type QuestionField = {
  name: string; title: string; description?: string; type: FieldType; required: boolean; enum?: FieldValue[];
  minLength?: number; maxLength?: number; minimum?: number; maximum?: number; multipleOf?: number;
};
export type QuestionForm = { fields: QuestionField[]; error?: never } | { fields?: never; error: string };
const rootKeys = new Set(['type', 'properties', 'required', 'title', 'description', 'additionalProperties', 'minProperties', 'maxProperties', '$schema', '$id']);
const fieldKeys = new Set(['type', 'title', 'description', 'default', 'enum', 'minLength', 'maxLength', 'pattern', 'format', 'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum', 'multipleOf']);
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function matches(value: unknown, type: FieldType): value is FieldValue {
  if (type === 'string' || type === 'boolean') return typeof value === type;
  return typeof value === 'number' && Number.isFinite(value) && (type === 'number' || Number.isSafeInteger(value));
}
export function questionFields(schema: unknown): QuestionForm {
  const unsupported = { error: '此问题的表单结构暂不支持；不能提交回答，请拒绝或取消。' };
  if (!object(schema) || schema.type !== 'object' || Object.keys(schema).some(key => !rootKeys.has(key))) return unsupported;
  if (schema.additionalProperties !== undefined && typeof schema.additionalProperties !== 'boolean') return unsupported;
  const properties = schema.properties === undefined ? {} : schema.properties;
  const required = schema.required === undefined ? [] : schema.required;
  if (!object(properties) || !Array.isArray(required) || required.some(name => typeof name !== 'string' || !Object.hasOwn(properties, name))) return unsupported;
  const fields: QuestionField[] = [];
  for (const [name, value] of Object.entries(properties)) {
    if (!object(value) || Object.keys(value).some(key => !fieldKeys.has(key))) return unsupported;
    if (!['string', 'boolean', 'integer', 'number'].includes(value.type as string)) return unsupported;
    const type = value.type as FieldType;
    if (value.enum !== undefined && (!Array.isArray(value.enum) || value.enum.length === 0 || value.enum.some(item => !matches(item, type)))) return unsupported;
    fields.push({
      name, type, title: typeof value.title === 'string' ? value.title : name,
      description: typeof value.description === 'string' ? value.description : undefined,
      required: required.includes(name), enum: value.enum as FieldValue[] | undefined,
      minLength: typeof value.minLength === 'number' ? value.minLength : undefined,
      maxLength: typeof value.maxLength === 'number' ? value.maxLength : undefined,
      minimum: typeof value.minimum === 'number' ? value.minimum : undefined,
      maximum: typeof value.maximum === 'number' ? value.maximum : undefined,
      multipleOf: typeof value.multipleOf === 'number' ? value.multipleOf : undefined,
    });
  }
  return { fields };
}
export function questionContent(fields: QuestionField[], values: Record<string, string>): Record<string, FieldValue> {
  const entries: [string, FieldValue][] = [];
  for (const field of fields) {
    const raw = Object.hasOwn(values, field.name) ? values[field.name] : '';
    if (raw === '' && (field.type !== 'string' || field.enum || !Object.hasOwn(values, field.name))) {
      if (field.required) throw new Error(`请填写「${field.title}」。`);
      continue;
    }
    let value: FieldValue;
    if (field.enum) {
      const index = Number(raw);
      if (!Number.isInteger(index) || !Object.hasOwn(field.enum, index)) throw new Error(`请选择「${field.title}」的选项。`);
      value = field.enum[index];
    } else if (field.type === 'boolean') {
      if (raw !== 'true' && raw !== 'false') throw new Error(`请选择「${field.title}」的是或否。`);
      value = raw === 'true';
    } else if (field.type === 'string') value = raw;
    else {
      value = raw.trim() ? Number(raw) : NaN;
      if (!matches(value, field.type)) throw new Error(`「${field.title}」需要有效的${field.type === 'integer' ? '整数' : '数字'}。`);
    }
    entries.push([field.name, value]);
  }
  return Object.fromEntries(entries);
}
export function mergeQuestions(previous: Question[], incoming: Question[], session: string, turn: string): Question[] {
  const next = [...previous];
  for (const question of incoming) {
    if (question.session !== session || question.turn !== turn) continue;
    const index = next.findIndex(item => item.id === question.id);
    if (index < 0) next.push(question);
    else if (next[index].status === 'pending') next[index] = question;
  }
  return next;
}
export function questionsPath(session: string, turn: string): string {
  return `/sessions/${encodeURIComponent(session)}/turns/${encodeURIComponent(turn)}/questions`;
}
export async function loadQuestionHistory(session: string, turns: string[], signal: AbortSignal): Promise<Question[]> {
  const questions = await Promise.all(turns.map(async turn => {
    const result = await api<{ questions: Question[] }>(questionsPath(session, turn), 'GET', undefined, signal);
    return mergeQuestions([], result.questions, session, turn);
  }));
  return questions.flat();
}
export async function answerQuestion(question: Question, answer: QuestionAnswer, signal: AbortSignal): Promise<Question> {
  const result = await api<{ question: Question }>(`${questionsPath(question.session, question.turn)}/${encodeURIComponent(question.id)}/answer`, 'POST', answer, signal);
  if (result.question.id !== question.id || result.question.session !== question.session || result.question.turn !== question.turn) {
    throw new Error('回答返回了不同的问题，请重新加载执行记录。');
  }
  return result.question;
}
