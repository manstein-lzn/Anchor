import { useRef, useState, type CSSProperties, type ReactNode } from 'react';

/** Widths belong to the workbench, so editing and execution use the same drag behavior. */
export function Workspace({ running = false, children }: { running?: boolean; children: ReactNode }) {
  const root = useRef<HTMLDivElement>(null);
  const drag = useRef<{ side: 'left' | 'right'; x: number; width: number } | null>(null);
  const [widths, setWidths] = useState<{ left?: number; right?: number }>({});
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
    const maximum = Math.max(minimum, Math.min(side === 'left' ? 480 : 720,
      area.clientWidth - otherWidth - 280));
    setWidths(previous => ({ ...previous, [side]: Math.round(Math.max(minimum, Math.min(maximum, width))) }));
  };
  return <div ref={root} className={`workspace ${running ? 'running' : ''}`} style={{
    '--sidebar-width': widths.left === undefined ? undefined : `${widths.left}px`,
    '--inspector-width': widths.right === undefined ? undefined : `${widths.right}px`,
  } as CSSProperties}>
    {children}
    {(['left', 'right'] as const).map(side => <div key={side}
      className={`panel-resizer resizer-${side}`} role="separator" tabIndex={0}
      aria-label={side === 'left' ? '调整侧边栏宽度' : '调整详情面板宽度'}
      aria-orientation="vertical" aria-valuemin={side === 'left' ? 160 : 220}
      aria-valuemax={side === 'left' ? 480 : 720}
      aria-valuenow={widths[side] ?? (side === 'left' ? (running ? 250 : 224) : (running ? 370 : 304))}
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
