import { useState } from 'react';
import type { GraphTaskPolicy, Plugin } from './model';
import { PluginSettings } from './PluginPanel';
import './GraphTaskPolicyEditor.css';

export function GraphTaskPolicyEditor({ value, onChange, graphs, catalog, catalogKnown, loading = false, disabled = false, onDetails }: {
  value: GraphTaskPolicy; onChange: (policy: GraphTaskPolicy) => void; graphs: string[];
  catalog: Plugin[]; catalogKnown: boolean; loading?: boolean; disabled?: boolean; onDetails: (plugin: Plugin) => void;
}) {
  const [query, setQuery] = useState('');
  const selectedGraphs = value.graphs ?? [];
  const selectedPlugins = value.plugins ?? [];
  const graphNames = [...new Set([...graphs, ...selectedGraphs])].sort((a, b) => a.localeCompare(b, 'zh-CN'));
  const plugins: Plugin[] = [...catalog.filter(plugin => plugin.id !== 'graph-tasks'),
    ...selectedPlugins.filter(id => !catalog.some(plugin => plugin.id === id) || id === 'graph-tasks')
      .map(id => ({ id, name: id, description: '', available: catalogKnown ? false : undefined,
        error: id === 'graph-tasks' ? '子任务不能继续委派 Graph 任务' : catalogKnown ? '插件未在库中找到' : undefined }))];
  const toggle = (items: string[], id: string) => items.includes(id) ? items.filter(item => item !== id) : [...items, id];
  return <section className="graph-task-policy" aria-label="Graph 任务授权">
    <h3>Graph 任务授权</h3>
    <p className="plugin-panel-note">仅能发现和调用勾选的工作流。所有授权默认关闭，保存后由宿主执行最终校验。</p>
    <fieldset disabled={disabled}>
      <legend>可见工作流</legend>
      <label className="graph-task-search">搜索工作流<input type="search" value={query}
        onChange={event => setQuery(event.target.value)} placeholder="按工作流名称筛选…" /></label>
      <div className="graph-task-targets">
        {graphNames.filter(name => name.toLocaleLowerCase().includes(query.toLocaleLowerCase())).map(name => <label key={name} className="graph-task-option">
          <input type="checkbox" checked={selectedGraphs.includes(name)} onChange={() => onChange({ ...value, graphs: toggle(selectedGraphs, name) })} />
          <span>{name}{!graphs.includes(name) && <small>当前列表中不可用，可取消授权</small>}</span>
        </label>)}
        {!graphNames.length && <p className="plugin-message">还没有其他工作流。可先创建，或授权此节点构建工作流。</p>}
        {!!graphNames.length && !graphNames.some(name => name.toLocaleLowerCase().includes(query.toLocaleLowerCase()))
          && <p className="plugin-message">没有匹配的工作流。</p>}
      </div>
      <label className="graph-task-option"><input type="checkbox" checked={value.create_graphs ?? false}
        onChange={event => onChange({ ...value, create_graphs: event.target.checked })} /><span>允许构建新的工作流</span></label>
      <label className="graph-task-option"><input type="checkbox" checked={value.develop_plugins ?? false}
        onChange={event => onChange({ ...value, develop_plugins: event.target.checked })} /><span>允许准备和测试候选插件</span></label>
    </fieldset>
    <fieldset disabled={disabled}>
      <legend>新建 Graph 能力上限</legend>
      <p className="plugin-panel-note">既有工作流按保存时批准的版本执行。以下限制用于新建工作流，不安装插件、不授予宿主凭据，也不修改当前节点配置。</p>
      {(['network', 'commands'] as const).map(field => <label key={field} className="graph-task-option">
        <input type="checkbox" checked={value[field] ?? false} onChange={event => onChange({ ...value, [field]: event.target.checked })} />
        <span>{field === 'network' ? '允许新建的子任务联网（不修改当前助手）' : '允许执行沙箱命令'}</span>
      </label>)}
    </fieldset>
    <h4>新建 Graph 可用插件</h4>
    <PluginSettings items={plugins} setting={plugin => ({ selected: selectedPlugins.includes(plugin.id), disabled })}
      onToggle={plugin => onChange({ ...value, plugins: toggle(selectedPlugins, plugin.id) })} onDetails={onDetails}
      loading={loading} selectedLabel="已授权" unselectedLabel="未授权" enableLabel="授权新建 Graph 使用" disableLabel="取消新建 Graph 授权"
      empty={<p className="plugin-message">插件库为空。安装后刷新，再选择可用于子任务的插件。</p>} />
  </section>;
}
