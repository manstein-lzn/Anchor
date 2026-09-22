import { useEffect, useState } from 'react';
import { Modal } from './ui';

let engine: Promise<typeof import('mermaid')['default']> | undefined;
let nextId = 0;
function loadMermaid() {
  return engine ??= import('mermaid').then(({ default: mermaid }) => {
    mermaid.initialize({
      startOnLoad: false, securityLevel: 'strict', suppressErrorRendering: true,
      theme: 'neutral', htmlLabels: false, flowchart: { htmlLabels: false },
      secure: ['securityLevel', 'startOnLoad', 'suppressErrorRendering', 'maxTextSize', 'maxEdges', 'htmlLabels'],
    });
    return mermaid;
  });
}

export function MermaidDiagram({ source }: { source: string }) {
  const [image, setImage] = useState('');
  const [error, setError] = useState('');
  const [expanded, setExpanded] = useState(false);
  useEffect(() => {
    let active = true;
    setImage(''); setError('');
    void loadMermaid().then(mermaid => mermaid.render(`anchor-diagram-${++nextId}`, source))
      .then(({ svg }) => {
        // An image cannot execute SVG scripts or inject HTML into the workbench DOM.
        if (active) setImage(`data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`);
      }).catch(() => {
        if (active) setError('图表无法渲染，请检查下方 Mermaid 源码。');
      });
    return () => { active = false; };
  }, [source]);
  return <figure className="mermaid-diagram">
    <figcaption><span>Mermaid</span>{image && <button onClick={() => setExpanded(true)}>放大图表</button>}</figcaption>
    {image ? <img src={image} alt="Mermaid 图表，文字定义见下方源码" />
      : <p className={error ? 'problem' : 'hint'} role={error ? 'alert' : 'status'}>{error || '正在绘制图表…'}</p>}
    <details key={error ? 'error' : 'source'} open={error ? true : undefined}>
      <summary>查看图表源码</summary><pre className="code"><code>{source}</code></pre>
    </details>
    {expanded && <Modal title="Mermaid 图表" close={() => setExpanded(false)}>
      <div className="mermaid-expanded"><img src={image} alt="Mermaid 图表" /></div>
    </Modal>}
  </figure>;
}
