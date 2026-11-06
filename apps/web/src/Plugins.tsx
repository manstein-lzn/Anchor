import { useEffect, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { api, reason } from './api';
import type { GraphPluginActivation, GraphTaskPolicy, Plugin } from './model';
import { PluginDetails, PluginSettings, pluginSettingStatus } from './PluginPanel';
import { LibraryPluginDetails } from './LibraryPluginDetails';
import { GraphTaskPolicyEditor } from './GraphTaskPolicyEditor';

/** Graph owns draft references; Run owns captured metadata; the Library owns current definitions. */
export function Plugins({ selected, onChange, disabled = false, recorded, management = false, onChanged, refreshRevision = 0, activationMode, graph, graphTasks }:
  { selected?: string[]; onChange?: (ids: string[]) => void; disabled?: boolean; recorded?: Plugin[]; management?: boolean; onChanged?: () => void; refreshRevision?: number; activationMode?: GraphPluginActivation['mode']; graph?: { name: string; saved: boolean };
    graphTasks?: { graphs: string[]; value: GraphTaskPolicy; onChange: (policy: GraphTaskPolicy) => void } }) {
  const [catalog, setCatalog] = useState<Plugin[]>([]);
  const [catalogKnown, setCatalogKnown] = useState(false);
  const [error, setError] = useState('');
  const [opened, setOpened] = useState<Plugin | null>(null);
  const [loading, setLoading] = useState(recorded === undefined);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (recorded !== undefined) return;
    let active = true;
    setLoading(true); setError('');
    api<{ plugins: Plugin[] }>('/plugins').then(data => {
      if (!Array.isArray(data.plugins)) throw new Error('插件列表响应无效，请重试。');
      if (active) { setCatalog(data.plugins); setCatalogKnown(true); }
    }).catch(problem => { if (active) setError(reason(problem)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [recorded, revision, refreshRevision]);
  useEffect(() => { setOpened(null); }, [recorded !== undefined]);
  type CatalogItem = Plugin & { unconfirmed?: boolean };
  const items: CatalogItem[] = recorded ?? [...catalog, ...(selected ?? []).filter(id => !catalog.some(plugin => plugin.id === id))
    .map(id => ({ id, name: id, description: '', available: catalogKnown ? false : undefined,
      error: catalogKnown ? '插件未在库中找到' : undefined, unconfirmed: !catalogKnown }))];
  const selectedIds = new Set(selected ?? []);
  const status = (plugin: CatalogItem) => pluginSettingStatus(plugin, { selected: selectedIds.has(plugin.id),
    status: plugin.unconfirmed ? '已选择 · 状态未确认' : undefined }, { recorded: recorded !== undefined, management });
  return <section className="plugin-panel plugins-panel" aria-label={recorded !== undefined ? '运行插件记录' : management ? '已安装插件' : '节点插件配置'}>
    <div className="plugin-panel-header"><div><p className="plugin-panel-context">{recorded !== undefined ? '本次运行' : management ? '共享库' : '节点能力'}</p><h3>插件</h3></div>
      {recorded === undefined && <button className="plugin-panel-refresh" type="button" disabled={loading}
        onClick={() => setRevision(value => value + 1)}><RefreshCw size={14} />刷新</button>}
    </div>
    <p className="plugin-panel-note">{recorded !== undefined ? '本次运行保存的绑定记录，不代表实际调用或任务成功。' : management ? '打开详情可管理凭据、更新版本或检查卸载影响。' : activationMode === 'next_turn' ? '保存后，各会话从下一条消息使用新配置；当前轮继续完成。' : '更改保存后用于后续运行。'}</p>
    <PluginSettings key={`${recorded !== undefined}/${management}`} items={items}
      setting={plugin => ({ selected: selectedIds.has(plugin.id), disabled, status: status(plugin) })}
      onToggle={onChange ? plugin => onChange(selectedIds.has(plugin.id)
        ? (selected ?? []).filter(id => id !== plugin.id) : [...(selected ?? []), plugin.id]) : undefined}
      onDetails={setOpened} loading={loading} error={error} retry={() => setRevision(value => value + 1)}
      recorded={recorded !== undefined} management={management}
      empty={<div className="plugin-empty"><strong>{recorded !== undefined ? '没有插件绑定记录' : '还没有安装插件'}</strong>
        <p>{recorded !== undefined ? '此运行没有保存插件绑定信息。' : management ? '从“安装或导入插件”选择插件目录或 GitHub 链接。' : '告诉 Pilot 你需要什么能力。安装完成后刷新，即可为节点选择插件。'}</p></div>} />
    {graphTasks && selectedIds.has('graph-tasks') && <GraphTaskPolicyEditor {...graphTasks} disabled={disabled}
      catalog={catalog} catalogKnown={catalogKnown} loading={loading} onDetails={setOpened} />}
    {opened && (recorded !== undefined ? <PluginDetails plugin={opened} status={status(opened)} recorded close={() => setOpened(null)} />
      : <LibraryPluginDetails key={opened.id} plugin={opened} status={status(opened)} management={management} graph={selectedIds.has(opened.id) ? graph : undefined}
        onChanged={() => { setRevision(value => value + 1); onChanged?.(); }} close={() => setOpened(null)} />)}
  </section>;
}
