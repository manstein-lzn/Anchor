import { useEffect, useRef, useState } from 'react';
import { api, human, reason } from './api';
import { Modal } from './ui';
import './PluginManagement.css';

type Coverage = { kind: 'complete' | 'partial' | 'unknown'; reason?: string; considered?: number; total?: number; missing?: { graph: string; reason: string }[] };
export type PluginReview = { state?: 'preparing' | 'ready' | 'running' | 'succeeded' | 'failed'; error?: string | null; result?: { installed?: boolean; uninstalled?: boolean; digest?: string }; token: string; operation: 'install' | 'uninstall'; plugin: string; name: string; digest: string;
  network?: boolean; mutable_paths?: string[]; authorization_digest?: string; expires_at: number; graphs: string[]; coverage: Coverage; refusal?: string | null;
  mutable?: { path: string; bytes: number | null }[]; sessions?: string[]; unfinished_runs?: string[];
  replacing?: boolean; already_installed?: boolean; binary_without_source?: boolean; warnings?: string[];
  resources?: string[]; resource_bytes?: number; skills?: string[]; servers?: { name: string; transport: string; environment: string[]; command?: string; remote_origin?: string; argument_count?: number; channel_identity?: string[] }[];
  channels?: { platform: string; required_environment: string[] }[]; dependencies?: unknown; existing_digest?: string | null };

function Impact({ review }: { review: PluginReview }) {
  return <><p>{review.graphs.length ? `引用此插件的 Graph：${review.graphs.join('、')}` : '当前未发现 Graph 引用。'}</p>
    {review.operation === 'uninstall' && !!review.graphs.length && <p>卸载后，这些 Graph 的下一次运行会因插件缺失而失败。</p>}
    {review.operation === 'install' && review.replacing && <p>更新后，正在运行的任务会在下一次节点调用使用新代码，已完成步骤不会重跑。</p>}
    {review.coverage.kind !== 'complete' && <p className="plugin-message is-error">影响范围未完整读取：{review.coverage.reason
      || `已检查 ${review.coverage.considered ?? 0}/${review.coverage.total ?? '?'} 个 Graph`}
      {review.coverage.missing?.map(item => `${item.graph}：${item.reason}`).join('；')}</p>}</>;
}
function ReviewIdentity({ review }: { review: PluginReview }) {
  return <><strong>{review.name} · {review.plugin}</strong><p>内容摘要：<code>{review.digest}</code></p>
    <p>本次预检有效至 {new Date(review.expires_at * 1000).toLocaleTimeString('zh-CN')}。确认时会重新检查版本与引用。</p></>;
}
function discard(token: string) { void api(`/plugins/preflights/${encodeURIComponent(token)}`, 'DELETE').catch(() => undefined); }

async function observe(initial: PluginReview, alive: () => boolean, progress: (value: PluginReview) => void): Promise<PluginReview> {
  let current = initial;
  while (alive() && (current.state === 'preparing' || current.state === 'running')) {
    progress(current);
    await new Promise(resolve => setTimeout(resolve, 700));
    if (!alive()) return current;
    current = await api<PluginReview>(`/plugins/preflights/${encodeURIComponent(current.token)}`);
  }
  if (current.state === 'failed') throw new Error(current.error || '插件操作未完成，请核对当前库后重新预检。');
  return current;
}


export function PluginUninstall({ pluginId, onChanged, initialReview }: { pluginId: string; onChanged?: () => void; initialReview?: PluginReview }) {
  const [review, setReview] = useState<PluginReview | null>(initialReview ?? null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState(false);
  const [dataLoss, setDataLoss] = useState(false);
  const alive = useRef(true);
  const token = useRef(initialReview?.token ?? '');
  useEffect(() => { alive.current = true; return () => { alive.current = false; if (token.current) discard(token.current); }; }, []);
  const prepare = async () => {
    setPending(true); setError(''); setConfirm(false); setDataLoss(false);
    if (token.current) { discard(token.current); token.current = ''; }
    try {
      const result = await api<PluginReview>(`/plugins/${encodeURIComponent(pluginId)}/uninstall-preflight`, 'POST');
      if (!alive.current) { discard(result.token); return; }
      token.current = result.token; setReview(result);
    } catch (problem) { if (alive.current) setError(reason(problem)); }
    finally { if (alive.current) setPending(false); }
  };
  const uninstall = async () => {
    if (!review || !confirm || !dataLoss || pending) return;
    setPending(true); setError(''); token.current = '';
    try {
      const result = await api<PluginReview>(`/plugins/preflights/${encodeURIComponent(review.token)}/confirm`, 'POST', { confirm: true, confirm_data_loss: true });
      await observe(result, () => alive.current, () => undefined);
      if (alive.current) { setReview(null); onChanged?.(); }
    } catch (problem) { if (alive.current) { setError(reason(problem)); setReview(null); } }
    finally { if (alive.current) setPending(false); }
  };
  return <section className="plugin-details-section"><div className="plugin-management-heading"><h3>卸载插件</h3>
    <button type="button" disabled={pending} onClick={() => void prepare()}>{pending ? '处理中…' : review ? '重新检查' : '查看卸载影响'}</button></div>
    {error && <p className="plugin-message is-error" role="alert">{error}</p>}
    {review && <form className="plugin-management-form plugin-management-review" onSubmit={event => { event.preventDefault(); void uninstall(); }}>
      <ReviewIdentity review={review} /><Impact review={review} />
      <p>卸载会删除插件、依赖视图和已保存的插件凭据。</p>
      {!!review.mutable?.length ? <><p>插件自己保存的数据也会永久删除：</p><ul>{review.mutable.map(item => <li key={item.path}><code>{item.path}</code> · {item.bytes === null ? '体积未知' : human(item.bytes)}</li>)}</ul></>
        : <p>此插件没有声明可变数据路径。</p>}
      {review.refusal ? <p className="plugin-message is-error" role="alert">{review.refusal}</p> : <>
        <label><input type="checkbox" checked={confirm} disabled={pending} onChange={event => setConfirm(event.target.checked)} />确认卸载插件 {review.plugin}</label>
        <label><input type="checkbox" checked={dataLoss} disabled={pending} onChange={event => setDataLoss(event.target.checked)} />确认删除这个插件唯一的一份数据，不可恢复</label>
        <button type="submit" disabled={pending || !confirm || !dataLoss}>{pending ? '卸载中…' : '确认卸载'}</button></>}
      <button type="button" disabled={pending} onClick={() => { if (token.current) discard(token.current); token.current = ''; setReview(null); }}>取消卸载</button>
    </form>}
  </section>;
}

async function uploadFiles(files: File[]) {
  if (!files.length || files.length > 2048) throw new Error('请选择包含 1–2048 个文件的插件目录。');
  if (files.reduce((size, file) => size + file.size, 0) > 32 * 1024 * 1024) throw new Error('插件目录超过 32 MiB，请移除构建缓存和非发布文件。');
  const result: { path: string; content_base64: string; executable: boolean }[] = [];
  for (const file of files) {
    const relative = file.webkitRelativePath || file.name;
    const path = file.webkitRelativePath ? relative.split('/').slice(1).join('/') : relative;
    const bytes = new Uint8Array(await file.arrayBuffer());
    let binary = '';
    for (let index = 0; index < bytes.length; index += 0x8000) binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
    result.push({ path, content_base64: btoa(binary), executable: path.startsWith('bin/') || path.endsWith('.sh') });
  }
  return result;
}

export function PluginInstall({ pluginId, onChanged, close, initialReview }: { pluginId?: string; onChanged?: () => void; close: () => void; initialReview?: PluginReview }) {
  const [id, setId] = useState(pluginId ?? '');
  const [mode, setMode] = useState<'directory' | 'github'>('directory');
  const [source, setSource] = useState('');
  const [files, setFiles] = useState<File[]>([]);
  const [network, setNetwork] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const [review, setReview] = useState<PluginReview | null>(initialReview ?? null);
  const [progress, setProgress] = useState<PluginReview | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [replacement, setReplacement] = useState(false);
  const [unreadable, setUnreadable] = useState(false);
  const alive = useRef(true); const token = useRef(initialReview?.token ?? '');
  useEffect(() => { alive.current = true; return () => { alive.current = false; if (token.current) discard(token.current); }; }, []);
  const prepare = async () => {
    if (pending) return;
    setPending(true); setError('');
    try {
      const input = mode === 'github' ? { source: source.trim() } : { files: await uploadFiles(files) };
      const result = await api<PluginReview>('/plugins/install-preflight', 'POST', { id: id.trim(), ...input, allow_network: network });
      if (!alive.current) { discard(result.token); return; }
      token.current = result.token;
      const prepared = await observe(result, () => alive.current, setProgress);
      if (!alive.current) return;
      setProgress(null); setReview(prepared); setConfirmed(false); setReplacement(false); setUnreadable(false);
    } catch (problem) { if (alive.current) setError(reason(problem)); }
    finally { if (alive.current) setPending(false); }
  };
  const publish = async () => {
    if (!review || pending) return;
    setPending(true); setError(''); token.current = '';
    try {
      const result = await api<PluginReview>(`/plugins/preflights/${encodeURIComponent(review.token)}/confirm`, 'POST', { confirm: confirmed,
        replace_existing: replacement, accept_unreadable: unreadable });
      await observe(result, () => alive.current, setProgress);
      if (alive.current) { onChanged?.(); close(); }
    } catch (problem) { if (alive.current) { setError(reason(problem)); setReview(null); } }
    finally { if (alive.current) setPending(false); }
  };
  return <Modal title={pluginId ? `更新插件 ${pluginId}` : '安装或导入插件'} close={close} className="plugin-details">
    {error && <p className="plugin-message is-error" role="alert">{error} 刷新插件库可核对当前状态和操作记录。</p>}
    {progress && <p className="plugin-message" role="status">{progress.state === 'preparing' ? '正在准备并预检插件…关闭窗口后，取消将在当前源码获取步骤结束时收束。' : '正在发布插件…可关闭窗口，稍后从插件库查看结果。'}</p>}
    {!review ? <form className="plugin-management-form" onSubmit={event => { event.preventDefault(); void prepare(); }}>
      <label>插件标识<input value={id} disabled={pending || !!pluginId} required pattern="[a-zA-Z0-9_][a-zA-Z0-9_.-]*" maxLength={200} onChange={event => setId(event.target.value)} placeholder="例如 my-plugin" /></label>
      <label>导入方式<select value={mode} disabled={pending} onChange={event => setMode(event.target.value as typeof mode)}><option value="directory">上传插件目录</option><option value="github">GitHub 目录链接</option></select></label>
      {mode === 'github' ? <label>GitHub tree URL<input type="url" value={source} disabled={pending} required onChange={event => setSource(event.target.value)} placeholder="https://github.com/owner/repo/tree/ref/path" /></label>
        : <><label>插件目录<input type="file" multiple ref={node => { node?.setAttribute('webkitdirectory', ''); }} disabled={pending}
          onChange={event => { const selected = Array.from(event.target.files ?? []); setFiles(selected);
            if (!pluginId && !id && selected[0]?.webkitRelativePath) setId(selected[0].webkitRelativePath.split('/')[0]); }} /></label>
          <p className="plugin-management-note">选择包含 plugin.json 的目录。最多 2048 个普通文件、32 MiB。bin/ 内的文件和 .sh 文件将设为可执行；不导入符号链接。</p>
          {!!files.length && <p className="plugin-management-note">已选择 {files.length} 个文件 · {human(files.reduce((size, file) => size + file.size, 0))}</p>}</>}
      <label><input type="checkbox" checked={network} disabled={pending} onChange={event => setNetwork(event.target.checked)} />允许这次预检与安装联网获取源码及部署已授权的依赖</label>
      <p className="plugin-management-note">预检只准备候选。核对内容、权限和影响后，再确认发布到共享库。</p>
      <button type="submit" disabled={pending || !id.trim() || (mode === 'github' ? !source.trim() || !network : !files.length)}>{pending ? '正在预检…' : '预检插件'}</button>
    </form> : <form className="plugin-management-form" onSubmit={event => { event.preventDefault(); void publish(); }}>
      <div className="plugin-management-review"><ReviewIdentity review={review} /><Impact review={review} />
        <p>运行联网需求：{review.network ? '需要授权的网络能力' : '未声明'}{review.mutable_paths?.length ? ` · 可写路径：${review.mutable_paths.join('、')}` : ' · 未声明可写路径'}</p>
        <p>{review.resources?.length ?? 0} 个发布文件 · {human(review.resource_bytes ?? 0)}{review.already_installed ? ' · 已安装相同内容，本次可修复依赖视图' : ''}</p>
        {!!review.skills?.length && <p>技能：{review.skills.join('、')}</p>}
        {!!review.servers?.length && <ul>{review.servers.map(server => <li key={server.name}>MCP {server.name} · {server.transport}{server.command ? ` · 入口 ${server.command}` : ''}{server.remote_origin ? ` · 远程服务 ${server.remote_origin}` : ''}{server.environment.length ? ` · 凭据字段 ${server.environment.join('、')}` : ''}{server.channel_identity?.length ? ` · 向该服务提供渠道成员身份 ${server.channel_identity.join('、')}` : ''}</li>)}</ul>}
        {!!review.channels?.length && <ul>{review.channels.map(channel => <li key={channel.platform}>渠道 {channel.platform} · {channel.required_environment.join('、')}</li>)}</ul>}
        {!!review.dependencies && <details><summary>已锁定的依赖</summary><pre>{JSON.stringify(review.dependencies, null, 2)}</pre></details>}
        <details><summary>发布文件清单</summary><ul>{review.resources?.map(path => <li key={path}><code>{path}</code></li>)}</ul></details>
        {review.warnings?.map(warning => <p className="plugin-message is-error" key={warning}>{warning}</p>)}
      </div>
      <label><input type="checkbox" checked={confirmed} disabled={pending} onChange={event => setConfirmed(event.target.checked)} />确认安装此摘要对应的插件</label>
      {review.replacing && <label><input type="checkbox" checked={replacement} disabled={pending} onChange={event => setReplacement(event.target.checked)} />确认覆盖已安装的 {review.plugin}（{review.existing_digest}）</label>}
      {review.binary_without_source && <label><input type="checkbox" checked={unreadable} disabled={pending} onChange={event => setUnreadable(event.target.checked)} />我接受没有可读源码的编译产物</label>}
      <div className="plugin-authorization-actions"><button type="submit" disabled={pending || !confirmed || !!review.replacing && !replacement || !!review.binary_without_source && !unreadable}>
        {pending ? '正在发布…' : review.replacing ? '确认更新插件' : '确认安装插件'}</button>
        <button type="button" disabled={pending} onClick={() => { if (token.current) discard(token.current); token.current = ''; setReview(null); }}>返回修改</button></div>
    </form>}
  </Modal>;
}


export function PluginOperations({ onChanged }: { onChanged: () => void }) {
  const [records, setRecords] = useState<PluginReview[]>([]);
  const [selected, setSelected] = useState<PluginReview | null>(null);
  const [error, setError] = useState('');
  const [revision, setRevision] = useState(0);
  const changed = useRef(onChanged); changed.current = onChanged;
  const states = useRef(new Map<string, string | undefined>());
  useEffect(() => {
    let alive = true; let timer: ReturnType<typeof setTimeout>;
    const load = async () => {
      try {
        const result = await api<{ reviews: PluginReview[] }>('/plugins/preflights');
        if (!alive) return;
        setRecords(result.reviews); setError('');
        if (result.reviews.some(record => (record.state === 'succeeded' || record.state === 'failed')
          && states.current.get(record.token) !== record.state)) changed.current();
        states.current = new Map(result.reviews.map(record => [record.token, record.state]));
      } catch (problem) { if (alive) setError(reason(problem)); }
      if (alive) timer = setTimeout(() => void load(), 1800);
    };
    void load(); return () => { alive = false; clearTimeout(timer); };
  }, [revision]);
  const finish = () => { setSelected(null); setRevision(current => current + 1); changed.current(); };
  const labels = { preparing: '正在预检', ready: '等待确认', running: '正在发布', succeeded: '已完成', failed: '未完成' };
  return <section className="plugin-details-section"><div className="plugin-management-heading"><h3>插件操作</h3>
    <button type="button" onClick={() => { setRevision(current => current + 1); changed.current(); }}>刷新库与操作状态</button></div>
    <p className="plugin-management-note">服务重启后，临时预检记录可能已释放。请打开插件详情核对当前摘要；没有记录不能说明未安装。</p>
    {error && <p className="plugin-message is-error" role="alert">{error}</p>}
    {records.map(record => <div className="plugin-management-review" key={record.token}>
      <strong>{record.plugin} · {record.operation === 'install' ? '安装 / 更新' : '卸载'} · {labels[record.state ?? 'ready']}</strong>
      {record.digest && <p>目标摘要：<code>{record.digest}</code></p>}
      {record.error && <p className="plugin-message is-error">{record.error}</p>}
      {record.result?.digest && <p>已安装摘要：<code>{record.result.digest}</code></p>}
      {record.state === 'ready' && <button type="button" onClick={() => setSelected(record)}>继续确认 {record.plugin}</button>}
      {record.state !== 'running' && <button type="button" onClick={() => {
        void api(`/plugins/preflights/${encodeURIComponent(record.token)}`, 'DELETE')
          .then(() => setRevision(current => current + 1)).catch(problem => setError(reason(problem)));
      }}>{record.state === 'preparing' || record.state === 'ready' ? '取消预检' : '关闭记录'}</button>}
    </div>)}
    {selected?.operation === 'install' && <PluginInstall initialReview={selected} pluginId={selected.plugin} close={() => { setSelected(null); setRevision(current => current + 1); }} onChanged={finish} />}
    {selected?.operation === 'uninstall' && <Modal title={`卸载插件 ${selected.plugin}`} close={() => setSelected(null)} className="plugin-details">
      <PluginUninstall pluginId={selected.plugin} initialReview={selected} onChanged={finish} /></Modal>}
  </section>;
}
