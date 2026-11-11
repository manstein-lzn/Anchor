import { useEffect, useState, type ReactNode } from 'react';
import { LoaderCircle, Search } from 'lucide-react';
import { Markdown } from './markdown';
import { Modal } from './ui';
import type { Plugin } from './model';
import './PluginPanel.css';

export type PluginInformation = {
  id: string; name: string; description?: string; skills?: string[]; digest?: string;
  kind?: Plugin['kind']; canUninstall?: boolean; canReplace?: boolean; updateSource?: Plugin['updateSource'];
  mcpServers?: Record<string, { transport: string; auth?: string }>;
  mcpServerNames?: string[]; instructions?: string; error?: string | null; unsupported?: string[]; channels?: { platform: string }[];
  available?: boolean; requiresNetwork?: boolean;
  definitionValid?: boolean; dependenciesReady?: boolean | null;
  contextAuthorized?: boolean | null; connectionVerified?: boolean | null;
};

export type PluginSetting = {
  selected: boolean; active?: boolean; required?: boolean; disabled?: boolean; pending?: boolean;
  status?: string; problem?: string; notice?: string;
};

export function pluginSettingStatus(plugin: PluginInformation, state: PluginSetting, {
  recorded = false, management = false, selectedLabel = '已选择', unselectedLabel = '未选择',
}: { recorded?: boolean; management?: boolean; selectedLabel?: string; unselectedLabel?: string } = {}): string {
  if (state.status) return state.status;
  if (recorded) return '运行时绑定记录';
  if (management) return plugin.available === false ? '不可用' : '已安装';
  const selected = state.active ?? state.selected;
  if (plugin.available === false) return selected ? `${selectedLabel} · 不可用` : '不可用';
  return selected ? selectedLabel : unselectedLabel;
}

/** The complete list interaction shared by draft selection and persisted mounts.
 * Loading and writes stay with their owners; this component never invents a saved fact. */
export function PluginSettings<T extends PluginInformation>({ items, setting, onToggle, onDetails, loading = false,
  error = '', retry, recorded = false, management = false, selectedLabel = '已选择', unselectedLabel = '未选择',
  enableLabel = '选择', disableLabel = '取消选择', empty, children, control }: {
  items: T[]; setting: (plugin: T) => PluginSetting; onToggle?: (plugin: T) => void;
  onDetails: (plugin: T) => void; loading?: boolean; error?: string; retry?: () => void;
  recorded?: boolean; management?: boolean; selectedLabel?: string; unselectedLabel?: string;
  enableLabel?: string; disableLabel?: string; empty: ReactNode; children?: (plugin: T) => ReactNode;
  control?: (plugin: T, setting: PluginSetting) => ReactNode;
}) {
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('all');
  const needle = query.trim().toLocaleLowerCase();
  const chosen = (plugin: T) => setting(plugin).active ?? setting(plugin).selected;
  const count = items.filter(chosen).length;
  const visible = items.filter(plugin => filter === 'all' || (filter === 'selected' ? chosen(plugin)
    : filter === 'unselected' ? !chosen(plugin) : plugin.available !== false))
    .filter(plugin => !needle || [plugin.name, plugin.id, plugin.description, ...(plugin.skills ?? []),
      ...Object.keys(plugin.mcpServers ?? {}), ...(plugin.mcpServerNames ?? [])]
      .some(value => value?.toLocaleLowerCase().includes(needle)))
    .sort((left, right) => (recorded || management ? 0 : Number(chosen(right)) - Number(chosen(left)))
      || left.name.localeCompare(right.name, 'zh-CN') || left.id.localeCompare(right.id));
  return <div className="plugin-settings" aria-busy={loading}>
    {!!items.length && <PluginToolbar query={query} onQuery={setQuery} filter={filter} onFilter={setFilter}
      filters={recorded ? [] : [{ value: 'all', label: '全部', count: items.length },
        ...(management ? [] : [{ value: 'selected', label: selectedLabel, count },
          { value: 'unselected', label: unselectedLabel, count: items.length - count }]),
        { value: 'available', label: '可用' }]} />}
    {loading && <div className="session-plugins-loading"><p className="plugin-message" role="status">
      <LoaderCircle size={16} className="spin" aria-hidden="true" />正在加载插件…</p>
      {!items.length && <div className="session-plugin-placeholders" aria-hidden="true">{[0, 1, 2].map(index =>
        <div className="session-plugin-placeholder" key={index}><span /><span /><span /></div>)}</div>}
    </div>}
    {error && <div className="plugin-message is-error" role="alert"><span>插件列表读取失败：{error}</span>
      {retry && <button type="button" onClick={retry}>重试</button>}</div>}
    {!loading && !error && !items.length && empty}
    {!!items.length && !visible.length && <div className="plugin-empty"><strong>没有匹配的插件</strong>
      <p>试试其他名称，或清空搜索和筛选。</p><button type="button" onClick={() => { setQuery(''); setFilter('all'); }}>查看全部插件</button></div>}
    <PluginCardList items={visible} scope={`${filter}/${query}`}>{plugin => {
      const state = setting(plugin);
      const unavailable = !recorded && plugin.available === false;
      const status = pluginSettingStatus(plugin, state, { recorded, management, selectedLabel, unselectedLabel });
      const customControl = control?.(plugin, state);
      return <PluginCard key={plugin.id} plugin={plugin} status={status} active={!recorded && chosen(plugin)} unavailable={unavailable}
        onDetails={() => onDetails(plugin)} control={!recorded && !management && (customControl !== undefined ? customControl : onToggle ?
          <button type="button" role="switch" aria-checked={state.selected}
            aria-label={`${state.selected ? disableLabel : enableLabel} ${plugin.name || plugin.id}`}
            className={`session-plugin-switch${state.selected ? ' on' : ''}`}
            disabled={state.disabled || state.required || state.pending || unavailable && !state.selected}
            title={state.required ? '工作流需要此插件，保持挂载' : undefined}
            aria-busy={state.pending} onClick={() => onToggle(plugin)}><span className="plugin-switch-track" aria-hidden="true" /></button> : undefined)}>
        {plugin.requiresNetwork && <p className="plugin-message">需要联网</p>}
        {state.required && <p className="plugin-message is-required">必需 · 工作流需要此插件，保持挂载。</p>}
        {unavailable && <p className="plugin-message is-error">不可用：{plugin.error || 'Host 未给出原因'}</p>}
        {!!plugin.unsupported?.length && <p className="plugin-message is-error">暂不支持：{plugin.unsupported.join('、')}</p>}
        {state.problem && <p className="plugin-message is-error" role="alert">{state.problem}</p>}
        {state.notice && !state.problem && <p className="plugin-message is-success" role="status">{state.notice}</p>}
        {children?.(plugin)}
      </PluginCard>;
    }}</PluginCardList>
  </div>;
}

/** Filtering applies to the complete catalog; only the visible card batch is mounted. */
export function PluginCardList<T>({ items, scope, className = '', children }: {
  items: T[]; scope: string; className?: string; children: (item: T) => ReactNode;
}) {
  const [limit, setLimit] = useState(50);
  useEffect(() => { setLimit(50); }, [scope]);
  return <>
    <div className={`plugin-card-list ${className}`}>{items.slice(0, limit).map(children)}</div>
    {items.length > limit && <div className="plugin-list-more"><span>已显示 {limit} / {items.length} 个插件</span>
      <button type="button" onClick={() => setLimit(current => current + 50)}>显示更多插件</button>
    </div>}
  </>;
}

/** Presentation only: selection, policy and library facts belong to the caller. */
export function PluginToolbar({ query, onQuery, filters = [], filter, onFilter }: {
  query: string; onQuery: (query: string) => void;
  filters?: { value: string; label: string; count?: number }[]; filter?: string;
  onFilter?: (filter: string) => void;
}) {
  return <div className="plugin-toolbar">
    <div className="plugin-search-control"><Search size={16} aria-hidden="true" />
      <input type="search" aria-label="搜索插件" placeholder="搜索名称或能力…" value={query}
        onChange={event => onQuery(event.target.value)} />
    </div>
    {!!filters.length && <div className="plugin-filter-control" role="group" aria-label="插件筛选">
      {filters.map(item => <button type="button" key={item.value} aria-pressed={filter === item.value}
        className={filter === item.value ? 'chosen' : ''} onClick={() => onFilter?.(item.value)}>
        {item.label}{item.count !== undefined && <span>{item.count}</span>}
      </button>)}
    </div>}
  </div>;
}

export function PluginCard({ plugin, status, active = false, unavailable = false, control, children, onDetails, className = '' }: {
  plugin: PluginInformation; status: string; active?: boolean; unavailable?: boolean;
  control?: ReactNode; children?: ReactNode; onDetails: () => void; className?: string;
}) {
  return <article className={`plugin-card ${className}${active ? ' attached' : ''}${unavailable ? ' unavailable' : ''}`}>
    <div className="plugin-card-heading"><h4 className="plugin-card-name">{plugin.name || plugin.id}</h4>
      {control && <div className="plugin-card-control">{control}</div>}
    </div>
    <p className="plugin-card-description">{plugin.description || '此插件尚未提供用途说明。'}</p>
    <div className="plugin-card-footer"><div className="plugin-card-meta"><span className={`plugin-card-status${unavailable ? ' is-unavailable' : ''}`}>{status}</span>
      {plugin.kind === 'system' && <span className="plugin-card-status">系统插件</span>}
      {children}</div>
      <button className="plugin-card-details" type="button" aria-label={`查看 ${plugin.name || plugin.id} 的详情`} onClick={onDetails}>详情</button>
    </div>
  </article>;
}

export function PluginDetails({ plugin, status, recorded = false, loading = false, error = '', retry, close, children }: {
  plugin: PluginInformation; status: string; recorded?: boolean; loading?: boolean; error?: string;
  retry?: () => void; close: () => void; children?: ReactNode;
}) {
  const servers = Object.keys(plugin.mcpServers ?? {});
  const serverNames = servers.length ? servers : plugin.mcpServerNames ?? [];
  return <Modal title={`${plugin.name || plugin.id} · 插件详情`} className="plugin-details" close={close}>
    <p className="plugin-details-source">{recorded ? '历史记录 · 仅展示本次运行保存的插件信息，不代表实际调用或任务成功。' : '当前插件库 · 展示当前插件定义。'}</p>
    <div className="plugin-details-overview"><span className="plugin-card-status">{status}</span>
      <p>{plugin.description || '此插件尚未提供用途说明。'}</p>
      <dl className="plugin-details-metadata"><div><dt>标识</dt><dd><code>{plugin.id}</code></dd></div>
        {plugin.kind && <div><dt>类型</dt><dd>{plugin.kind === 'system' ? '系统插件' : '扩展插件'}</dd></div>}
        {plugin.updateSource && <div><dt>更新来源</dt><dd>{plugin.updateSource === 'anchor-release' ? 'Anchor 升级' : '插件版本'}</dd></div>}
        {plugin.digest && <div><dt>内容摘要</dt><dd><code>{plugin.digest}</code></dd></div>}
      </dl>
    </div>
    {!recorded && plugin.updateSource === 'anchor-release' && <p className="plugin-message">此系统能力由 Anchor 提供，随 Anchor 升级更新。</p>}
    {plugin.error && <p className="plugin-message is-error">不可用：{plugin.error}</p>}
    {!!plugin.unsupported?.length && <p className="plugin-message is-error">暂不支持：{plugin.unsupported.join('、')}</p>}
    {loading && <p className="plugin-message" role="status">正在读取插件详情…</p>}
    {error && <div className="plugin-message is-error" role="alert"><span>插件详情读取失败：{error}</span>
      {retry && <button type="button" onClick={retry}>重试</button>}</div>}
    <section className="plugin-details-section"><h3>技能 <span>Skills</span></h3>
      {plugin.skills?.length ? <ul>{plugin.skills.map((skill, index) => <li key={`${skill}-${index}`}>{skill}</li>)}</ul>
        : <p>{plugin.skills !== undefined ? '未声明技能。' : recorded ? '本次运行未保存技能信息。'
          : loading ? '正在读取技能信息…' : error ? '技能信息暂时无法读取。' : '未声明技能。'}</p>}
    </section>
    <section className="plugin-details-section"><h3>工具服务 <span>MCP</span></h3>
      {serverNames.length ? <ul>{serverNames.map(name => <li key={name}><strong>{name}</strong>
        {plugin.mcpServers?.[name]?.transport && <span className="plugin-details-transport">{plugin.mcpServers[name].transport}</span>}
      </li>)}</ul> : <p>{plugin.mcpServers !== undefined || plugin.mcpServerNames !== undefined ? '未声明工具服务。'
        : recorded ? '本次运行未保存工具服务信息。' : loading ? '正在读取工具服务信息…'
          : error ? '工具服务信息暂时无法读取。' : '未声明工具服务。'}</p>}
    </section>
    {children}
    <section className="plugin-details-section"><h3>使用说明</h3>
      {plugin.instructions ? <Markdown text={plugin.instructions} prefix={`plugin-${plugin.id}`}
        fileBase={recorded ? undefined : `/plugins/${encodeURIComponent(plugin.id)}/files/`} />
        : <p>{recorded ? '本次运行未保存使用说明。' : loading ? '正在读取使用说明…'
          : error ? '使用说明暂时无法读取。请重试。' : '此插件未提供使用说明。'}</p>}
    </section>
  </Modal>;
}
