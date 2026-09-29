import { useEffect, useState } from 'react';
import { BookOpen, RefreshCw, Search, X } from 'lucide-react';
import { api, reason } from './api';
import type { Plugin } from './model';
import { Markdown } from './markdown';

let catalogCache: Plugin[] | null = null;
let catalogRequest: Promise<Plugin[]> | null = null;
type PluginFilter = 'all' | 'attached' | 'available';

const loadCatalog = (): Promise<Plugin[]> => {
  if (catalogCache) return Promise.resolve(catalogCache);
  catalogRequest ??= api<{ plugins: Plugin[] }>('/plugins')
    .then(data => { catalogCache = data.plugins; return data.plugins; })
    .finally(() => { catalogRequest = null; });
  return catalogRequest;
};

/** A node owns references; definitions are read from the shared file-backed library. */
export function Plugins({ selected, onChange, disabled = false, recorded }:
  { selected?: string[]; onChange?: (ids: string[]) => void; disabled?: boolean; recorded?: Plugin[] }) {
  const [catalog, setCatalog] = useState<Plugin[]>(() => catalogCache ?? []);
  const [error, setError] = useState('');
  const [opened, setOpened] = useState<Plugin | null>(null);
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<PluginFilter>('all');
  const [installSource, setInstallSource] = useState('');
  const [installing, setInstalling] = useState(false);
  const [authorizing, setAuthorizing] = useState('');
  useEffect(() => {
    if (recorded !== undefined) return;
    if (catalogCache) return;
    let active = true;
    setLoading(true);
    setError('');
    loadCatalog().then(data => {
      if (active) setCatalog(data);
    }).catch(e => { if (active) setError(reason(e)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [recorded, revision]);
  const items: Plugin[] = recorded ?? [...catalog, ...(selected ?? []).filter(id => !catalog.some(p => p.id === id))
    .map(id => ({ id, name: id, description: '', available: false, error: 'Plugin 未在库中找到' }))];
  const selectedIds = new Set(selected ?? []);
  const attached = items.filter(plugin => selectedIds.has(plugin.id));
  const needle = query.trim().toLocaleLowerCase();
  const visible = items
    .filter(plugin => filter === 'attached' ? selectedIds.has(plugin.id)
      : filter === 'available' ? plugin.available !== false : true)
    .filter(plugin => !needle || [plugin.name, plugin.id, plugin.description]
      .some(value => value.toLocaleLowerCase().includes(needle)))
    .sort((left, right) => Number(selectedIds.has(right.id)) - Number(selectedIds.has(left.id))
      || left.name.localeCompare(right.name, 'zh-CN'));
  const read = async (id: string) => {
    setError('');
    try { setOpened(await api<Plugin>(`/plugins/${encodeURIComponent(id)}`)); }
    catch (e) { setError(reason(e)); }
  };
  return <section className="plugins-panel" aria-label="Plugin 能力">
    <div className="plugins-header"><div>
      <div className="plugins-kicker">Agent 能力</div>
      <h3><BookOpen size={15} />Plugin</h3>
    </div>
      {recorded === undefined && <button className="icon-text-button" type="button" disabled={loading} onClick={() => {
        catalogCache = null;
        setRevision(v => v + 1);
      }}><RefreshCw size={13} />刷新</button>}
    </div>
    {recorded !== undefined && <p className="inspector-note">本次运行解析的 Plugin 记录；不代表实际调用或任务成功。</p>}
    {onChange && attached.length > 0 && <div className="plugin-attached">
      <div className="plugin-subheading">已挂载</div>
      <div className="plugin-chips">{attached.map(plugin => <span className="plugin-chip" key={plugin.id}>
        <span title={plugin.id}>{plugin.name}</span>
        <button type="button" aria-label={`移除 ${plugin.name}`} disabled={disabled}
          onClick={() => onChange((selected ?? []).filter(id => id !== plugin.id))}><X size={12} /></button>
      </span>)}</div>
    </div>}
    {!loading && items.length > 0 && <div className="plugin-browser">
      <label className="plugin-search"><Search size={14} /><input aria-label="搜索 Plugin" value={query}
        placeholder="搜索能力名称、标识或说明…" onChange={event => setQuery(event.target.value)} /></label>
      <div className="plugin-filters" role="group" aria-label="Plugin 筛选">
        {([['all', '全部'], ['attached', '已挂载'], ['available', '可用']] as [PluginFilter, string][]).map(([value, label]) =>
          <button key={value} type="button" className={filter === value ? 'chosen' : ''}
            aria-pressed={filter === value} onClick={() => setFilter(value)}>{label}</button>)}
      </div>
    </div>}
    {recorded === undefined && onChange && <form className="plugin-install" onSubmit={async event => {
      event.preventDefault(); setInstalling(true); setError('');
      try {
        await api('/plugins/install', 'POST', { source: installSource });
        catalogCache = null; setInstallSource(''); setRevision(v => v + 1);
        setCatalog(await loadCatalog());
      } catch (e) { setError(reason(e)); } finally { setInstalling(false); }
    }}>
      <label>安装社区 Plugin
        <input type="url" required placeholder="https://github.com/owner/repo/tree/ref/path"
          value={installSource} onChange={event => setInstallSource(event.target.value)} />
      </label>
      <button type="submit" disabled={installing}>{installing ? '安装中…' : '安装'}</button>
      <small>Skills/资源与 MCP 可用；hooks、commands、agents 暂不执行。</small>
    </form>}
    {loading && <p role="status">正在读取 Plugin 库…</p>}
    {error && <p role="alert" className="plugin-error">{error}</p>}
    {!loading && !items.length && <p className="inspector-note">{recorded !== undefined
      ? '此运行没有 Plugin 绑定记录。' : '还没有 Plugin。在共享库中添加定义后刷新，即可为节点挂载。'}</p>}
    {!loading && items.length > 0 && !visible.length && <p className="inspector-note">没有匹配的 Plugin。</p>}
    <div className="plugin-list">{visible.map(plugin => {
      const isAttached = selectedIds.has(plugin.id);
      return <article className={`plugin-row${isAttached ? ' attached' : ''}${plugin.available === false ? ' unavailable' : ''}`} key={plugin.id}>
        {onChange ? <label className="plugin-row-label"><input type="checkbox" checked={isAttached}
          disabled={disabled || plugin.available === false && !selected?.includes(plugin.id)}
          onChange={event => onChange(event.target.checked ? [...(selected ?? []), plugin.id]
            : (selected ?? []).filter(id => id !== plugin.id))} /><span className="plugin-row-copy">
              <strong>{plugin.name}</strong><small>{plugin.id}</small>
              {plugin.description && <span>{plugin.description}</span>}
            </span></label> : <div className="plugin-row-label"><strong>{plugin.name}</strong><small>{plugin.id}</small></div>}
        <div className="plugin-row-actions">
          {!!plugin.skills?.length && <span className="plugin-tool-count">{plugin.skills.length} Skills</span>}
          {recorded === undefined && Object.entries(plugin.mcpServers ?? {}).filter(([, server]) => server.auth === 'oauth')
            .map(([name]) => <button key={name} type="button" disabled={!!authorizing}
              onClick={async () => {
                setAuthorizing(`${plugin.id}/${name}`); setError('');
                try { await api(`/plugins/${encodeURIComponent(plugin.id)}/authorize/${encodeURIComponent(name)}`, 'POST'); }
                catch (e) { setError(reason(e)); } finally { setAuthorizing(''); }
              }}>{authorizing === `${plugin.id}/${name}` ? '等待授权…' : `授权 ${name}`}</button>)}
          {recorded === undefined && plugin.available !== false
            && <button type="button" onClick={() => void read(plugin.id)}>查看说明</button>}
        </div>
        {plugin.error && <p className="plugin-error">不可用：{plugin.error}</p>}
        {!!plugin.unsupported?.length && <p className="plugin-error">暂不支持：{plugin.unsupported.join('、')}</p>}
        {recorded !== undefined && plugin.digest && <small className="plugin-digest">内容摘要：{plugin.digest.slice(0, 12)}</small>}
      </article>;
    })}</div>
    {opened && <div className="plugin-instructions">
      <div className="section-heading"><h3>{opened.name}</h3><button type="button" onClick={() => setOpened(null)}>关闭说明</button></div>
      <p className="inspector-note">来自当前共享库，文件维护，只读查看。</p>
      <Markdown text={opened.instructions ?? ''} prefix={`plugin-${opened.id}`}
        fileBase={`/plugins/${encodeURIComponent(opened.id)}/files/`} />
    </div>}
  </section>;
}
