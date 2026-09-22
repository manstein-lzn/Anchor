/** What a node left in its workspace, and a way to take it away.
 *
 * A node's output is exactly what is in its directory, so this is the other half of reading a run: the
 * conversation says what it did, and this says what came of it. The listing is reached over the API
 * `anchor-serve` already exposes, and a file is downloaded as itself rather than rendered — a workspace
 * holds whatever the node wrote, and a browser rendering it would be rendering text this program did
 * not write inside a page this program does serve.
 */

import { useEffect, useMemo, useState } from 'react';
import { AlertTriangle, ChevronRight, Download, FileText, LoaderCircle, RefreshCw } from 'lucide-react';
import { Markdown } from './markdown';
import { api, human, reason } from './api';
import type { OurFile, OurFileBody, OurFileList } from './model';

/** Grouped by directory, because a workspace is a tree and a flat list of ninety paths is not read. */
export function byDirectory(files: OurFile[]): { directory: string; items: OurFile[] }[] {
  const groups = new Map<string, OurFile[]>();
  for (const file of files) {
    const cut = file.path.lastIndexOf('/');
    const directory = cut === -1 ? '' : file.path.slice(0, cut);
    groups.set(directory, [...(groups.get(directory) ?? []), file]);
  }
  return [...groups].map(([directory, items]) => ({ directory, items }));
}

function Preview({ body }: { body: OurFileBody }) {
  if (body.binary) {
    return <p className="hint">
      这是二进制文件（{human(body.size)}），不能当文本看——下载它。
    </p>;
  }
  // Markdown only where the node wrote Markdown. Anything else is shown as the text it is, monospaced,
  // because rendering a log as prose would reflow it into something the node did not write.
  const markdown = /\.(md|markdown)$/i.test(body.path);
  return <>
    {markdown
      ? <div className="file-preview markdown-body"><Markdown text={body.text} prefix={body.path} /></div>
      : <pre className="file-preview">{body.text}</pre>}
    {body.truncated && <p className="folded">只显示了开头一部分，下载可以拿到全部。</p>}
  </>;
}

export function Files({ run, node }: { run: string; node: string }) {
  const [listing, setListing] = useState<OurFileList | null>(null);
  const [open, setOpen] = useState<string>('');
  const [body, setBody] = useState<OurFileBody | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);

  const load = async () => {
    setBusy(true);
    setProblem('');
    try {
      setListing(await api<OurFileList>(`/runs/${run}/files/${node}`));
    } catch (error) {
      setProblem(reason(error));
    } finally {
      setBusy(false);
    }
  };
  // Reloaded when the node changes, and when a poll brings a new run: what is listed is what was there
  // when it was asked for, and a node that is still working is still adding to it.
  useEffect(() => {
    setOpen(''); setBody(null);
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run, node]);

  const show = async (path: string) => {
    setOpen(path);
    setBody(null);
    try {
      setBody(await api<OurFileBody>(`/runs/${run}/files/${node}/${path}`));
    } catch (error) {
      setProblem(reason(error));
    }
  };

  const groups = useMemo(() => byDirectory(listing?.files ?? []), [listing]);

  return <div className="files">
    <div className="files-head">
      <span className="hint">{listing ? `${listing.files.length} 个文件` : '载入中'}</span>
      <button className="icon-button" title="刷新" disabled={busy} onClick={() => void load()}>
        {busy ? <LoaderCircle size={14} className="spin" /> : <RefreshCw size={14} />}
      </button>
    </div>
    {problem && <div className="message error" role="alert">
      <AlertTriangle size={15} /><span>{problem}</span>
    </div>}
    {listing && !listing.files.length && <p className="hint">
      这个节点还没有在工作区里留下文件。
    </p>}
    {listing?.truncated && <p className="folded">文件太多，这里只列了前面一部分。</p>}

    <div className="file-groups">
      {groups.map(group => <div className="file-group" key={group.directory || '.'}>
        {group.directory && <div className="file-directory">{group.directory}/</div>}
        {group.items.map(file => {
          const name = file.path.slice(file.path.lastIndexOf('/') + 1);
          return <div className={`file-row ${open === file.path ? 'open' : ''}`} key={file.path}>
            <button className="file-name" onClick={() => void show(file.path)}>
              <ChevronRight size={12} className="call-caret" />
              <FileText size={13} />
              <span title={file.path}>{name}</span>
              <small>{human(file.size)}</small>
            </button>
            <a className="file-get" title={`下载 ${name}`}
               href={`/runs/${run}/files/${node}/${file.path}?download=1`} download>
              <Download size={13} />
            </a>
          </div>;
        })}
      </div>)}
    </div>

    {open && <div className="file-open">
      <div className="file-open-head">
        <code>{open}</code>
        <a className="inline-link" href={`/runs/${run}/files/${node}/${open}?download=1`} download>
          下载
        </a>
      </div>
      {!body && !problem && <p className="hint"><LoaderCircle size={13} className="spin" /> 载入中</p>}
      {body && <Preview body={body} />}
    </div>}
  </div>;
}
