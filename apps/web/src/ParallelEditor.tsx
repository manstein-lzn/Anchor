import type { OurGraph } from './model';
import { pairedNodes, parallelControl } from './parallel';

export function ParallelEditor({ graph, node, onChange }: { graph: OurGraph; node: string; onChange: (join: string) => void }) {
  const fanout = parallelControl(graph, node) === 'fanout';
  const pairs = pairedNodes(graph, node);
  const joins = graph.nodes.filter(item => parallelControl(graph, item.id) === 'join');
  return <section aria-label="并行区域设置">
    <div className="inspector-kind">{fanout ? '并行展开' : '等待收束'}</div>
    <p className="inspector-note">同一工作流、同一次运行。各分支在自己的节点工作区执行，全部成功后才继续。</p>
    {fanout ? <label>配对收束节点<select value={pairs[0] ?? ''} onChange={event => onChange(event.target.value)}>
      {!joins.some(item => item.id === pairs[0]) && <option value={pairs[0] ?? ''}>{pairs[0] ? `${pairs[0]}（不存在）` : '请选择收束节点'}</option>}
      {joins.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
    </select></label> : <p>配对展开节点：{pairs.join('、') || '尚未配对'}{pairs.length > 1 && '（重复配对，请修正）'}</p>}
    <p className="inspector-note">展开节点至少连接两条互不重叠的串行分支，各分支连接到配对收束节点；收束后连接一个后续节点。首期不支持分支内选择、循环或嵌套并行。保存时会校验区域。</p>
    <p className="inspector-note">分支失败会停止仍在执行的同伴；已完成结果保留，后续节点不会启动。</p>
  </section>;
}
