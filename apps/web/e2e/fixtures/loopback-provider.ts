import { createServer } from 'node:http';

export type ProviderRequest = {
  stream: boolean;
  messages: Array<{ role: string; content: unknown }>;
  tools: Array<{ function: { name: string } }>;
};

type ProviderReply = { text?: string; tool?: { name: string; arguments: unknown } };

export function observedField(value: unknown, name: string): unknown {
  if (typeof value === 'string') {
    try { return observedField(JSON.parse(value), name); } catch { return undefined; }
  }
  if (Array.isArray(value)) return value.map(item => observedField(item, name)).find(item => item !== undefined);
  if (value && typeof value === 'object') {
    const object = value as Record<string, unknown>;
    if (object.type === 'text' && typeof object.text === 'string') {
      const nested = observedField(object.text, name);
      if (nested !== undefined) return nested;
    }
    if (object[name] !== undefined) return object[name];
    return Object.values(object).map(item => observedField(item, name)).find(item => item !== undefined);
  }
}

export function toolResultField(body: ProviderRequest, name: string) {
  return body.messages.filter(message => message.role === 'tool').reverse()
    .map(message => observedField(message.content, name)).find(value => value !== undefined);
}

export async function loopbackProvider(reply: (body: ProviderRequest, position: number) => ProviderReply | Promise<ProviderReply>) {
  const calls: ProviderRequest[] = [], failures: string[] = [];
  const provider = createServer(async (incoming, response) => {
    if (incoming.method === 'GET' && incoming.url === '/v1/models') {
      response.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ object: 'list',
        data: [{ id: 'fixture-browser', object: 'model', created: 1, owned_by: 'fixture' }] }));
      return;
    }
    try {
      if (incoming.method !== 'POST' || incoming.url !== '/v1/chat/completions') throw new Error('unexpected Provider route');
      let raw = '';
      for await (const chunk of incoming) raw += chunk.toString();
      const body: ProviderRequest = JSON.parse(raw);
      calls.push(body);
      if (body.stream !== true) throw new Error('streaming Provider request required');
      const planned = await reply(body, calls.length);
      const deltas: Record<string, unknown>[] = [];
      if (planned.text !== undefined) deltas.push({ role: 'assistant', content: planned.text });
      if (planned.tool) {
        const tools = body.tools.filter(tool => tool.function.name.endsWith(`__${planned.tool!.name}`));
        if (tools.length !== 1) throw new Error(`missing unique authorized ${planned.tool.name}`);
        deltas.push({ role: 'assistant', tool_calls: [{ index: 0, id: `fixture-${calls.length}`, type: 'function',
          function: { name: tools[0].function.name, arguments: JSON.stringify(planned.tool.arguments) } }] });
      }
      const common = { id: `fixture-${calls.length}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-browser' };
      const chunks = [...deltas.map(delta => ({ ...common, choices: [{ index: 0, delta, finish_reason: null }] })),
        { ...common, choices: [{ index: 0, delta: {}, finish_reason: planned.tool ? 'tool_calls' : 'stop' }],
          usage: { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 } }];
      response.writeHead(200, { 'Content-Type': 'text/event-stream' })
        .end(chunks.map(chunk => `data: ${JSON.stringify(chunk)}\n\n`).join('') + 'data: [DONE]\n\n');
    } catch (error) {
      failures.push((error as Error).message);
      response.writeHead(400).end('{}');
    }
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  if (!address || typeof address === 'string') throw new Error('missing Provider port');
  return {
    url: `http://127.0.0.1:${address.port}/v1`, calls, failures,
    async close() {
      provider.closeAllConnections();
      await new Promise<void>(resolve => provider.close(() => resolve()));
    },
  };
}
