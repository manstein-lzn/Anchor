import { useEffect, useState } from 'react';
import { api, reason } from './api';
import { previewCallInput } from './calls';
import { JsonDialog } from './ui';
import { callModeLabel, type GraphCall, type OurGraph } from './model';

type Session = { id: string; title: string; graph: string; platform: string };
export function CallEditor({ call, graph, node, targets, onChange, onOpen }: {
  call: GraphCall; graph: OurGraph; node: string; targets: string[];
  onChange: (call: GraphCall) => void; onOpen: (target: string) => void;
}) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [target, setTarget] = useState<OurGraph | null>(null);
  const [problem, setProblem] = useState('');
  const [sessionError, setSessionError] = useState('');
  const [constants, setConstants] = useState(false);
  useEffect(() => {
    let active = true;
    void api<{ sessions: Session[] }>('/channel-sessions').then(value => { if (active) setSessions(value.sessions); })
      .catch(error => { if (active) setSessionError(reason(error)); });
    return () => { active = false; };
  }, []);
  useEffect(() => {
    let active = true;
    setTarget(null); setProblem('');
    if (call.graph) void api<{ definition: OurGraph }>(`/graphs/${encodeURIComponent(call.graph)}`)
      .then(value => { if (active) setTarget(value.definition); })
      .catch(error => { if (active) setProblem(`目标不可读取：${reason(error)}`); });
    return () => { active = false; };
  }, [call.graph]);
  const update = (fields: Partial<GraphCall>) => onChange({ ...call, ...fields });
  const inputs = graph.nodes.filter(item => item.id !== node);
  const maps = Object.entries(call.input_map ?? {});
  let preview: string;
  try { preview = JSON.stringify(previewCallInput(call, graph.input), null, 2); }
  catch (error) { preview = reason(error); }
  return <section className="call-editor" aria-label="调用工作流设置">
    <label>目标工作流<select value={call.graph} onChange={event => update({ graph: event.target.value, session: undefined, result: undefined })}>
      <option value="">选择已安装工作流</option>
      {call.graph && !targets.includes(call.graph) && <option value={call.graph}>{call.graph}（引用已失效）</option>}
      {targets.map(name => <option key={name}>{name}</option>)}
    </select></label>
    {(!call.graph || !targets.includes(call.graph)) && <p className="problem" role="alert">请选择有效的目标工作流。</p>}
    {problem && <p className="problem" role="alert">{problem}</p>}
    <button className="full-button" disabled={!call.graph} onClick={() => onOpen(call.graph)}>打开目标工作流 ↗</button>
    <label>执行模式<select value={call.mode} onChange={event => update({ mode: event.target.value as GraphCall['mode'], ...(event.target.value === 'detach' ? { result: undefined } : {}) })}>
      <option value="wait">等待完成</option><option value="detach">启动后继续</option>
    </select></label>
    <p className="call-explanation">独立运行 · {callModeLabel(call.mode)}<br />{call.mode === 'wait'
      ? `此步骤等待「${call.graph || '目标'}」完成，再执行后续节点。停止本次运行也会停止它创建的目标运行。`
      : `「${call.graph || '目标'}」被接纳后，本流程继续。停止本次运行不会停止已经接纳的目标运行。`}</p>
    <h3>传入内容</h3>
    <button className="full-button" onClick={() => setConstants(true)}>编辑输入常量（JSON）</button>
    <pre className="call-json">{JSON.stringify(call.input ?? {}, null, 2)}</pre>
    <p className="inspector-note">常量覆盖目标默认输入；只传递下面明确选定的参数和文件。</p>
    <h4>当前运行输入 → 目标参数</h4>
    {maps.map(([key, pointer], index) => <div className="call-row" key={index}>
      <label>来源 JSON Pointer<input value={pointer} placeholder="/request/code" onChange={event => update({ input_map: Object.fromEntries(maps.map((row, i) => i === index ? [key, event.target.value] : row)) })} /></label>
      <label>目标参数<input value={key} placeholder="code" onChange={event => update({ input_map: Object.fromEntries(maps.map((row, i) => i === index ? [event.target.value, pointer] : row)) })} /></label>
      <button aria-label={`删除输入映射 ${index + 1}`} onClick={() => update({ input_map: Object.fromEntries(maps.filter((_, i) => i !== index)) })}>移除</button>
    </div>)}
    <button className="full-button" disabled={maps.some(([key]) => !key)} onClick={() => update({ input_map: { ...call.input_map, '': '' } })}>添加输入映射</button>
    <details><summary>预览传参（使用当前工作流默认输入）</summary><pre className="call-json">{preview}</pre></details>
    <h4>上游已提交文件</h4>
    {(call.files ?? []).map((file, index) => <div className="call-row" key={index}>
      <label>来源节点<select value={file.node} onChange={event => update({ files: call.files!.map((item, i) => i === index ? { ...item, node: event.target.value } : item) })}>
        <option value="">选择节点</option>{inputs.map(item => <option key={item.id}>{item.id}</option>)}
      </select></label>
      {(['path', 'as'] as const).map(field => <label key={field}>{field === 'path' ? '文件路径' : '目标文件名'}<input list={field === 'path' ? `call-source-files-${index}` : undefined} value={file[field]} placeholder={field === 'path' ? 'report.md' : 'report.md'} onChange={event => update({ files: call.files!.map((item, i) => i === index ? { ...item, [field]: event.target.value } : item) })} /></label>)}
      <datalist id={`call-source-files-${index}`}>{(() => { const source = graph.nodes.find(item => item.id === file.node); return (graph.ops?.[source?.op ?? '']?.writes ?? graph.agents?.[source?.agent ?? '']?.writes ?? []).map(path => <option key={path} value={path} />); })()}</datalist>
      <button aria-label={`删除传入文件 ${index + 1}`} onClick={() => update({ files: call.files!.filter((_, i) => i !== index) })}>移除</button>
    </div>)}
    <button className="full-button" onClick={() => update({ files: [...call.files ?? [], { node: '', path: '', as: '' }] })}>添加文件</button>
    <p className="inspector-note">运行时只允许当前节点可见的上游快照。目标从 /in/call/目标文件名 读取。</p>
    {call.mode === 'wait' && <>
      <h3>返回结果</h3>
      <label>目标输出节点<select value={call.result?.node ?? ''} onChange={event => update({ result: event.target.value ? { node: event.target.value, files: [] } : undefined })}>
        <option value="">只返回运行状态</option>
        {call.result?.node && !target?.nodes.some(item => item.id === call.result?.node) && <option>{call.result.node}</option>}
        {target?.nodes.map(item => <option key={item.id}>{item.id}</option>)}
      </select></label>
      {call.result && <label>返回文件（每行一个路径）<textarea value={call.result.files?.join('\n') ?? ''} placeholder="report.md" onChange={event => update({ result: { ...call.result!, files: event.target.value.split('\n') } })} /></label>}
      <p className="inspector-note">选定文件返回到本节点 result/ 目录；call.json 保留目标运行引用。</p>
    </>}
    <h3>助手会话（可选）</h3>
    <label>已有通道会话<select value={call.session ?? ''} onChange={event => update({ session: event.target.value || undefined })}>
      <option value="">独立运行，不使用会话</option>
      {call.session && !sessions.some(item => item.id === call.session) && <option value={call.session}>{call.session}（不可用）</option>}
      {sessions.filter(item => item.graph === call.graph).map(item => <option key={item.id} value={item.id}>{item.title || item.id} · {item.platform}</option>)}
    </select></label>
    {sessionError && <p className="inspector-note">无法读取会话：{sessionError}</p>}
    <p className="inspector-note">仅列出绑定目标工作流的已有会话，同一会话按顺序处理。</p>
    {constants && <JsonDialog title="输入常量" value={call.input ?? {}} close={() => setConstants(false)} apply={value => {
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('输入常量必须是 JSON object');
      update({ input: value as Record<string, unknown> });
    }} />}
  </section>;
}
