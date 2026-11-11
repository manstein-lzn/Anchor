import { api } from './api';
import type { OurAgent } from './model';

/** Model policy belongs to the Host; authoring preserves explicit legacy aliases. */
export async function defaultModelReference(): Promise<string> {
  const status = await api<{ execution: { default_reference: string } }>('/settings/model');
  const reference = status.execution?.default_reference;
  if (typeof reference !== 'string' || !reference.trim()) throw new Error('无法读取默认模型，请检查模型设置后重试。');
  return reference;
}

export function createAgent(model: string): OurAgent {
  return { model, instructions: '' };
}
