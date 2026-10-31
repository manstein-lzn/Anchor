import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { Maximize2, Minimize2, PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen } from 'lucide-react';
import { ToolButton } from './ui';

type Widths = { left?: number; right?: number };
type WorkspaceControls = {
  openInspector: () => void; sidebarControl: ReactNode; inspectorControls: ReactNode; restoreControls: ReactNode;
};
const WIDTHS_KEY = 'anchor.workspace.widths';

/** Each page owns panel visibility; widths and controls share the same behavior. */
export function Workspace({ running = false, sidebar = false, inspector = true, selection, children }: {
  running?: boolean; sidebar?: boolean; inspector?: boolean; selection?: unknown;
  children: ReactNode | ((controls: WorkspaceControls) => ReactNode);
}) {
  const root = useRef<HTMLDivElement>(null);
  const drag = useRef<{ side: 'left' | 'right'; x: number; width: number } | null>(null);
  const [widths, setWidths] = useState<Widths>(() => {
    try {
      const saved = JSON.parse(localStorage.getItem(WIDTHS_KEY) ?? '{}');
      return Object.fromEntries(['left', 'right'].filter(side =>
        typeof saved?.[side] === 'number' && Number.isFinite(saved[side]) && saved[side] > 0)
        .map(side => [side, saved[side]]));
    } catch { return {}; }
  });
  const [maximized, setMaximized] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const inspectedSelection = useRef(selection);
  useEffect(() => {
    if (selection !== inspectedSelection.current) {
      inspectedSelection.current = selection;
      if (selection) setInspectorOpen(true);
    }
  }, [selection]);
  useEffect(() => {
    try { localStorage.setItem(WIDTHS_KEY, JSON.stringify(widths)); }
    catch { /* Resizing still works when browser storage is unavailable. */ }
  }, [widths]);
  const resize = (side: 'left' | 'right', width: number) => {
    const area = root.current;
    if (!area) return;
    const other = area.querySelector<HTMLElement>(side === 'left' ? '.inspector' : '.library, .runs');
    const panel = area.querySelector<HTMLElement>(side === 'left' ? '.library, .runs' : '.inspector');
    const otherBox = other?.getBoundingClientRect();
    const panelBox = panel?.getBoundingClientRect();
    const otherWidth = otherBox && panelBox && otherBox.bottom > panelBox.top && otherBox.top < panelBox.bottom
      ? otherBox.width : 0;
    const minimum = side === 'left' ? 160 : 220;
    const maximum = Math.max(minimum, Math.min(side === 'left' ? 480 : Infinity,
      area.clientWidth - otherWidth - 280));
    setWidths(previous => ({ ...previous, [side]: Math.round(Math.max(minimum, Math.min(maximum, width))) }));
  };
  const sidebarControl = <ToolButton icon={PanelLeftClose} label="收起工作流列表" onClick={() => setSidebarOpen(false)} />;
  const inspectorControls = <div className="panel-heading-actions">
    <ToolButton icon={maximized ? Minimize2 : Maximize2} label={maximized ? '恢复详情面板' : '最大化详情面板'}
      aria-pressed={maximized} onClick={() => setMaximized(value => !value)} />
    <ToolButton icon={PanelRightClose} label="收起详情面板" onClick={() => { setInspectorOpen(false); setMaximized(false); }} />
  </div>;
  const restoreControls = <>
    {sidebar && !sidebarOpen && <ToolButton icon={PanelLeftOpen} label="展开工作流列表" onClick={() => setSidebarOpen(true)} />}
    {inspector && !inspectorOpen && <ToolButton icon={PanelRightOpen} label="展开详情面板" onClick={() => setInspectorOpen(true)} />}
  </>;
  return <div ref={root} className={`workspace ${running ? 'running' : ''} ${!sidebar || !sidebarOpen ? 'hide-library' : ''} ${!inspector || !inspectorOpen ? 'hide-inspector' : ''} ${maximized ? 'inspector-maximized' : ''}`} style={{
    '--sidebar-width': widths.left === undefined ? undefined : `${widths.left}px`,
    '--inspector-width': widths.right === undefined ? undefined : `${widths.right}px`,
  } as CSSProperties}>
    {typeof children === 'function' ? children({ openInspector: () => setInspectorOpen(true), sidebarControl, inspectorControls, restoreControls }) : children}
    {(['left', 'right'] as const).filter(side => side === 'left' ? sidebar && sidebarOpen : inspector && inspectorOpen).map(side => <div key={side}
      className={`panel-resizer resizer-${side}`} role="separator" tabIndex={0}
      aria-label={side === 'left' ? '调整侧边栏宽度' : '调整详情面板宽度'}
      aria-orientation="vertical" aria-valuemin={side === 'left' ? 160 : 220}
      aria-valuemax={side === 'left' ? 480 : undefined}
      aria-valuenow={widths[side] ?? (side === 'left' ? 220 : 300)}
      title="拖动调整宽度，双击恢复默认；方向键微调"
      onDoubleClick={() => setWidths(previous => ({ ...previous, [side]: undefined }))}
      onPointerDown={event => {
        if (event.button !== 0) return;
        const panel = root.current?.querySelector(side === 'left' ? '.library, .runs' : '.inspector');
        if (!panel) return;
        event.preventDefault();
        event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        drag.current = { side, x: event.clientX, width: panel.getBoundingClientRect().width };
      }}
      onPointerMove={event => {
        if (!drag.current || !event.currentTarget.hasPointerCapture(event.pointerId)) return;
        resize(side, drag.current.width + (event.clientX - drag.current.x) * (side === 'left' ? 1 : -1));
      }}
      onPointerUp={event => { drag.current = null; event.currentTarget.releasePointerCapture(event.pointerId); }}
      onLostPointerCapture={() => { drag.current = null; }}
      onKeyDown={event => {
        if (!['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
        event.preventDefault();
        const panel = root.current?.querySelector(side === 'left' ? '.library, .runs' : '.inspector');
        resize(side, (panel?.getBoundingClientRect().width ?? 0)
          + (event.key === 'ArrowRight' ? 16 : -16) * (side === 'left' ? 1 : -1));
      }} />)}
  </div>;
}
