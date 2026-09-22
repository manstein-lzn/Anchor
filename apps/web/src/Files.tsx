import { useEffect, useMemo, useState, useId } from 'react';
import { AlertTriangle, ChevronRight, Download, FileText, Folder, LoaderCircle, RefreshCw } from 'lucide-react';
import { Markdown } from './markdown';
import { api, human, reason } from './api';
import type { OurFile, OurFileBody, OurFileList } from './model';

type Directory = { name: string; directories: Map<string, Directory>; files: OurFile[] };

/** Paths become real nested folders, including folders with no immediate files. */
export function fileTree(files: OurFile[]): Directory {
  const root: Directory = { name: '', directories: new Map(), files: [] };
  for (const file of files) {
    let parent = root;
    for (const name of file.path.split('/').slice(0, -1)) {
      if (!parent.directories.has(name)) parent.directories.set(name, { name, directories: new Map(), files: [] });
      parent = parent.directories.get(name)!;
    }
    parent.files.push(file);
  }
  return root;
}

function Preview({ body, url }: { body: OurFileBody; url: string }) {
  if (body.binary) return <p className="hint">这是二进制文件（{human(body.size)}），请下载查看。</p>;
  return <>
    {/\.(md|markdown)$/i.test(body.path)
      ? <div className="file-preview markdown-body"><Markdown text={body.text} prefix={body.path} fileBase={url} /></div>
      : <pre className="file-preview">{body.text}</pre>}
    {body.truncated && <p className="folded">这里只显示开头部分，下载可查看完整内容。</p>}
  </>;
}

function FileItem({ file, base, revision }: { file: OurFile; base: string; revision: number }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<OurFileBody | null>(null);
  const [problem, setProblem] = useState('');
  const [retry, setRetry] = useState(0);
  const contentId = useId();
  const name = file.path.split('/').at(-1)!;
  const url = `${base}/${file.path.split('/').map(encodeURIComponent).join('/')}`;
  useEffect(() => {
    if (!open) return;
    let active = true;
    setBody(null); setProblem('');
    void api<OurFileBody>(url)
      .then(value => { if (active) setBody(value); })
      .catch(error => { if (active) setProblem(reason(error)); });
    return () => { active = false; };
  }, [open, url, revision, retry]);
  return <div className="file-item">
    <div className={`file-row ${open ? 'open' : ''}`}>
      <button className="file-name" aria-expanded={open} aria-controls={contentId}
        onClick={() => setOpen(value => !value)}>
        <ChevronRight size={12} className="call-caret" /><FileText size={13} />
        <span title={file.path}>{name}</span><small>{human(file.size)}</small>
      </button>
      <a className="file-get" aria-label={`下载 ${name}`} title={`下载 ${name}`} href={`${url}?download=1`} download>
        <Download size={13} />
      </a>
    </div>
    {open && <div className="file-open" id={contentId}>
      {problem ? <div className="message error" role="alert">
        <AlertTriangle size={15} /><span>{problem}</span>
        <button onClick={() => setRetry(value => value + 1)}>重试</button>
      </div> : body ? <Preview body={body} url={url} />
        : <p className="hint" role="status"><LoaderCircle size={13} className="spin" /> 载入中</p>}
    </div>}
  </div>;
}

function FolderContents({ directory, base, revision }: { directory: Directory; base: string; revision: number }) {
  return <>
    {[...directory.directories.values()].sort((a, b) => a.name.localeCompare(b.name)).map(child =>
      <details className="file-folder" key={child.name}>
        <summary><ChevronRight size={13} className="call-caret" /><Folder size={15} /><span>{child.name}</span></summary>
        <div className="file-folder-children"><FolderContents directory={child} base={base} revision={revision} /></div>
      </details>)}
    {directory.files.map(file => <FileItem key={file.path} file={file} base={base} revision={revision} />)}
  </>;
}

export function Files({ run, node }: { run: string; node: string }) {
  const [listing, setListing] = useState<OurFileList | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const base = `/runs/${encodeURIComponent(run)}/files/${node.split('/').map(encodeURIComponent).join('/')}`;
  useEffect(() => {
    let active = true;
    setBusy(true); setProblem('');
    void api<OurFileList>(base)
      .then(value => { if (active) setListing(value); })
      .catch(error => { if (active) setProblem(reason(error)); })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [base, revision]);
  const tree = useMemo(() => fileTree(listing?.files ?? []), [listing]);
  return <div className="files">
    <div className="files-head">
      <span className="hint">{listing ? `${listing.files.length} 个文件` : '载入中'}</span>
      <button className="icon-button" aria-label="刷新文件" title="刷新文件" disabled={busy}
        onClick={() => setRevision(value => value + 1)}>
        {busy ? <LoaderCircle size={14} className="spin" /> : <RefreshCw size={14} />}
      </button>
    </div>
    {problem && <div className="message error" role="alert"><AlertTriangle size={15} /><span>{problem}</span></div>}
    {listing && !listing.files.length && <p className="hint">这个节点还没有在工作区里留下文件。</p>}
    {listing?.truncated && <p className="folded">文件太多，这里只列了前面一部分。</p>}
    <div className="file-groups" key={base}><FolderContents directory={tree} base={base} revision={revision} /></div>
  </div>;
}
