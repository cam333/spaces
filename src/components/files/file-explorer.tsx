'use client';

import { useState, useEffect, useCallback, useRef } from 'react';
import {
  Folder, File, FileText, FileCode, FileJson, Image,
  ChevronRight, ChevronDown, X, ArrowUp, Upload,
} from 'lucide-react';
import { api } from '@/lib/api';

interface FileItem {
  name: string;
  path: string;
  isDir: boolean;
  ext: string | null;
}

function FileIconEl({ item }: { item: FileItem }) {
  if (item.isDir) return <Folder className="w-3.5 h-3.5 text-amber-500/70 flex-shrink-0" />;
  const ext = item.ext || '';
  if (['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg', '.ico', '.bmp'].includes(ext))
    return <Image className="w-3.5 h-3.5 text-emerald-400/70 flex-shrink-0" />;
  if (['.ts', '.tsx', '.js', '.jsx', '.py', '.rb', '.go', '.rs'].includes(ext))
    return <FileCode className="w-3.5 h-3.5 text-blue-400/70 flex-shrink-0" />;
  if (ext === '.json') return <FileJson className="w-3.5 h-3.5 text-green-400/70 flex-shrink-0" />;
  if (['.md', '.txt', '.yml', '.yaml'].includes(ext))
    return <FileText className="w-3.5 h-3.5 text-zinc-400/70 flex-shrink-0" />;
  return <File className="w-3.5 h-3.5 text-zinc-500/70 flex-shrink-0" />;
}

interface Props {
  onClose: () => void;
  navigateTo?: string | null;
  onOpenFile?: (file: { path: string; name: string; ext: string }) => void;
  activeFilePath?: string;
}

export function FileExplorer({ onClose, navigateTo, onOpenFile, activeFilePath }: Props) {
  const [devDirs, setDevDirs] = useState<string[]>([]);
  const [rootPath, setRootPath] = useState<string>('');
  const [currentPath, setCurrentPath] = useState('');
  const [items, setItems] = useState<FileItem[]>([]);
  const [parentPath, setParentPath] = useState<string | null>(null);
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);

  // Fetch dev directories on mount
  useEffect(() => {
    fetch(api('/api/config'))
      .then(r => r.json())
      .then(data => {
        const dirs: string[] = data.devDirectories || [];
        setDevDirs(dirs);
        if (dirs.length > 0 && !rootPath) {
          setRootPath(dirs[0]);
          setCurrentPath(dirs[0]);
        }
      })
      .catch(() => {});
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const switchRoot = (dir: string) => {
    setRootPath(dir);
    setCurrentPath(dir);
    setExpandedDirs(new Set());
  };

  // Navigate to a specific directory from outside (e.g. pane CWD click)
  useEffect(() => {
    if (!navigateTo) return;
    const matchingRoot = devDirs.find(d =>
      navigateTo === d || navigateTo.startsWith(d + '/') || navigateTo.startsWith(d + '\\')
    );
    if (matchingRoot) {
      setRootPath(matchingRoot);
      setCurrentPath(navigateTo);
    } else {
      setRootPath(navigateTo);
      setCurrentPath(navigateTo);
    }
    setExpandedDirs(new Set());
  }, [navigateTo, devDirs]);

  const loadDir = useCallback(async (dirPath: string) => {
    try {
      const res = await fetch(api(`/api/files?path=${encodeURIComponent(dirPath)}`));
      const data = await res.json();
      return { items: data.items || [], parent: data.parent || null };
    } catch {
      return { items: [], parent: null };
    }
  }, []);

  useEffect(() => {
    if (!currentPath) return;
    setLoading(true);
    loadDir(currentPath).then(data => {
      setItems(data.items);
      setParentPath(data.parent);
      setLoading(false);
    });
  }, [currentPath, loadDir]);

  const handleClick = (item: FileItem) => {
    if (item.isDir) {
      setExpandedDirs(prev => {
        const next = new Set(prev);
        if (next.has(item.path)) next.delete(item.path);
        else next.add(item.path);
        return next;
      });
    } else {
      onOpenFile?.({ path: item.path, name: item.name, ext: item.ext || '' });
    }
  };

  const navigateUp = () => {
    if (parentPath && currentPath !== rootPath) {
      setCurrentPath(parentPath);
    }
  };

  const uploadRef = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState(false);
  const [dragOver, setDragOver] = useState(false);

  const uploadFiles = async (files: FileList | File[]) => {
    if (!currentPath || files.length === 0) return;
    setUploading(true);
    try {
      const formData = new FormData();
      formData.append('dir', currentPath);
      for (const f of Array.from(files)) {
        formData.append('files', f);
      }
      await fetch(api('/api/files'), { method: 'POST', body: formData });
      const data = await loadDir(currentPath);
      setItems(data.items);
    } catch { /* upload failed */ }
    finally { setUploading(false); }
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    if (e.dataTransfer.files.length > 0) uploadFiles(e.dataTransfer.files);
  };

  const relativePath = rootPath ? currentPath.replace(rootPath, '').replace(/^[/\\]/, '') : '';
  const crumbs = relativePath ? relativePath.split(/[/\\]/) : [];
  const canGoUp = parentPath && currentPath !== rootPath;

  if (!rootPath) {
    return (
      <div className="h-full flex flex-col bg-zinc-950 border-r border-zinc-800">
        <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-800/50 flex-shrink-0">
          <span className="text-[10px] text-zinc-500 uppercase tracking-wider font-medium">Files</span>
          <button onClick={onClose} className="p-0.5 text-zinc-600 hover:text-zinc-300 transition-colors">
            <X className="w-3.5 h-3.5" />
          </button>
        </div>
        <div className="p-3 text-xs text-zinc-500">
          {devDirs.length === 0
            ? 'No development directories configured. Add them in Settings.'
            : 'Loading...'}
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col bg-zinc-950 border-r border-zinc-800">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-800/50 flex-shrink-0">
        <span className="text-[10px] text-zinc-500 uppercase tracking-wider font-medium">Files</span>
        <button onClick={onClose} className="p-0.5 text-zinc-600 hover:text-zinc-300 transition-colors">
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Root selector (when multiple dev dirs) */}
      {devDirs.length > 1 && (
        <div className="flex items-center gap-1 px-3 py-1.5 border-b border-zinc-800/30 flex-shrink-0 overflow-x-auto">
          {devDirs.map(dir => {
            const name = dir.split(/[/\\]/).pop() || dir;
            const isActive = dir === rootPath;
            return (
              <button
                key={dir}
                onClick={() => switchRoot(dir)}
                className={`px-2 py-0.5 text-[10px] rounded transition-colors flex-shrink-0 ${
                  isActive
                    ? 'bg-amber-500/15 text-amber-400 border border-amber-500/30'
                    : 'text-zinc-500 hover:text-zinc-300 border border-transparent'
                }`}
                title={dir}
              >
                {name}
              </button>
            );
          })}
        </div>
      )}

      {/* Breadcrumb */}
      <div className="flex items-center gap-1 px-3 py-1.5 border-b border-zinc-800/30 overflow-x-auto text-[10px] flex-shrink-0">
        {canGoUp && (
          <button onClick={navigateUp} className="text-zinc-500 hover:text-zinc-300 flex-shrink-0">
            <ArrowUp className="w-3 h-3" />
          </button>
        )}
        <input ref={uploadRef} type="file" multiple className="hidden" onChange={e => { if (e.target.files) uploadFiles(e.target.files); e.target.value = ''; }} />
        <button
          onClick={() => uploadRef.current?.click()}
          className="text-zinc-600 hover:text-zinc-300 flex-shrink-0 ml-auto"
          title="Upload files to current directory"
        >
          <Upload className="w-3 h-3" />
        </button>
        <button
          onClick={() => setCurrentPath(rootPath)}
          className="text-zinc-500 hover:text-zinc-300 truncate flex-shrink-0"
        >
          {rootPath.split(/[/\\]/).pop()}
        </button>
        {crumbs.map((crumb, i) => (
          <span key={i} className="flex items-center gap-1 flex-shrink-0">
            <span className="text-zinc-700">/</span>
            <button
              onClick={() => {
                const target = rootPath + '/' + crumbs.slice(0, i + 1).join('/');
                setCurrentPath(target);
              }}
              className="text-zinc-500 hover:text-zinc-300"
            >
              {crumb}
            </button>
          </span>
        ))}
      </div>

      {/* Tree */}
      <div
        className={`flex-1 overflow-y-auto ${dragOver ? 'ring-2 ring-inset ring-amber-500/50 bg-amber-500/5' : ''}`}
        onDragOver={e => { e.preventDefault(); setDragOver(true); }}
        onDragLeave={() => setDragOver(false)}
        onDrop={handleDrop}
      >
        {uploading ? (
          <div className="p-3 text-xs text-amber-400">Uploading...</div>
        ) : loading ? (
          <div className="p-3 text-xs text-zinc-600">Loading...</div>
        ) : items.length === 0 ? (
          <div className="p-3 text-xs text-zinc-600">Empty directory</div>
        ) : (
          <div className="py-1">
            {items.map(item => (
              <TreeItem
                key={item.path}
                item={item}
                depth={0}
                expanded={expandedDirs.has(item.path)}
                expandedDirs={expandedDirs}
                onClick={handleClick}
                loadDir={loadDir}
                activeFile={activeFilePath}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function TreeItem({
  item, depth, expanded, expandedDirs, onClick, loadDir, activeFile,
}: {
  item: FileItem;
  depth: number;
  expanded: boolean;
  expandedDirs: Set<string>;
  onClick: (item: FileItem) => void;
  loadDir: (path: string) => Promise<{ items: FileItem[]; parent: string | null }>;
  activeFile?: string;
}) {
  const [children, setChildren] = useState<FileItem[] | null>(null);

  useEffect(() => {
    if (expanded && item.isDir && !children) {
      loadDir(item.path).then(data => setChildren(data.items));
    }
  }, [expanded, item.isDir, item.path, children, loadDir]);

  const isActive = activeFile === item.path;

  return (
    <>
      <button
        onClick={() => onClick(item)}
        className={`flex items-center gap-1.5 w-full text-left px-2 py-[3px] text-[11px] transition-colors ${
          isActive ? 'bg-indigo-500/10 text-indigo-300' : 'text-zinc-400 hover:bg-zinc-800/50 hover:text-zinc-200'
        }`}
        style={{ paddingLeft: `${8 + depth * 14}px` }}
      >
        {item.isDir ? (
          expanded
            ? <ChevronDown className="w-3 h-3 text-zinc-600 flex-shrink-0" />
            : <ChevronRight className="w-3 h-3 text-zinc-600 flex-shrink-0" />
        ) : (
          <span className="w-3 flex-shrink-0" />
        )}
        <FileIconEl item={item} />
        <span className="truncate">{item.name}</span>
      </button>
      {expanded && item.isDir && children && children.map(child => (
        <TreeItem
          key={child.path}
          item={child}
          depth={depth + 1}
          expanded={expandedDirs.has(child.path)}
          expandedDirs={expandedDirs}
          onClick={onClick}
          loadDir={loadDir}
          activeFile={activeFile}
        />
      ))}
    </>
  );
}
