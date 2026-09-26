/** References the Pilot writes in a reply, and the pages they open.
 *
 * A link is `#anchor/<kind>/<id…>`: a plain in-page anchor, so it survives sanitizing, never becomes
 * a network fetch, and degrades to a no-op in any other renderer. The identifiers are the ones the
 * Pilot tools return -- the same Graph name, Run id and node path the workbench uses. */

export type AnchorRef =
  | { kind: 'graph'; graph: string }
  | { kind: 'run'; run: string }
  | { kind: 'artifact'; run: string; node: string; path: string };

export const ANCHOR_PREFIX = '#anchor/';

const decode = (value: string) => {
  try { return decodeURIComponent(value); } catch { return value; }
};

/** What page a link points at, or null when it is an ordinary link. */
export function anchorRef(href: string | undefined | null): AnchorRef | null {
  if (!href || !href.startsWith(ANCHOR_PREFIX)) return null;
  const [kind, ...rest] = href.slice(ANCHOR_PREFIX.length).split('/');
  if (kind === 'graph' && rest.length === 1 && rest[0]) return { kind: 'graph', graph: decode(rest[0]) };
  if (kind === 'run' && rest.length === 1 && rest[0]) return { kind: 'run', run: decode(rest[0]) };
  if (kind === 'artifact' && rest.length >= 3 && rest[0] && rest[1]) {
    return { kind: 'artifact', run: decode(rest[0]), node: decode(rest[1]),
             path: rest.slice(2).map(decode).join('/') };
  }
  return null;
}

/** The link for one reference, in the form the Pilot is told to write. */
export function anchorHref(ref: AnchorRef): string {
  const part = (value: string) => encodeURIComponent(value);
  if (ref.kind === 'graph') return `${ANCHOR_PREFIX}graph/${part(ref.graph)}`;
  if (ref.kind === 'run') return `${ANCHOR_PREFIX}run/${part(ref.run)}`;
  return `${ANCHOR_PREFIX}artifact/${part(ref.run)}/${part(ref.node)}/${ref.path.split('/').map(part).join('/')}`;
}

/** Where a reference takes the workbench. */
export function anchorTarget(ref: AnchorRef, graphOfRun: string) {
  if (ref.kind === 'graph') return { view: 'graph' as const, graph: ref.graph, run: '', node: '' };
  return { view: 'runs' as const, graph: graphOfRun, run: ref.run,
           node: ref.kind === 'artifact' ? ref.node : '',
           path: ref.kind === 'artifact' ? ref.path : '' };
}
