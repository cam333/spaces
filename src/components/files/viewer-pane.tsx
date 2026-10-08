'use client';

import { useEffect, useRef, useState } from 'react';
import { X, Maximize2, Minimize2, Minus, GripVertical, FileText, FileCode, FileJson, File as FileIcon, Image as ImageIcon, Loader2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { ColorPicker } from '@/components/common/color-picker';
import { FileViewerContent, langClass, formatSize, type FileContent } from './file-viewer-content';

import type { PaneData, ViewerTab } from '@/lib/db/queries';

interface Props {
  pane: PaneData;
  onClose: (id: string) => void;
  onUpdate: (id: string, data: Partial<PaneData>) => void;
  isMaximized: boolean;
  onToggleMaximize: (id: string) => void;
  onMinimize?: (id: string) => void;
  dragHandleProps?: Record<string, unknown>;
}

function tabIcon(ext: string) {
  if (['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg', '.ico', '.bmp'].includes(ext))
    return <ImageIcon className="w-3 h-3 text-emerald-400/70 flex-shrink-0" />;
  if (['.ts', '.tsx', '.js', '.jsx', '.py', '.rb', '.go', '.rs'].includes(ext))
    return <FileCode className="w-3 h-3 text-blue-400/70 flex-shrink-0" />;
  if (ext === '.json') return <FileJson className="w-3 h-3 text-green-400/70 flex-shrink-0" />;
  if (['.md', '.txt', '.yml', '.yaml'].includes(ext))
    return <FileText className="w-3 h-3 text-zinc-400/70 flex-shrink-0" />;
  return <FileIcon className="w-3 h-3 text-zinc-500/70 flex-shrink-0" />;
}

export function ViewerPane({ pane, onClose, onUpdate, isMaximized, onToggleMaximize, onMinimize, dragHandleProps }: Props) {
  const tabs = pane.viewerTabs || [];
  const activeIdx = Math.min(pane.viewerActiveTab ?? 0, Math.max(0, tabs.length - 1));
  const activeTab: ViewerTab | undefined = tabs[activeIdx];

  const [content, setContent] = useState<FileContent | null>(null);
  const [loading, setLoading] = useState(false);
  const [showColorPicker, setShowColorPicker] = useState(false);
  const colorPickerRef = useRef<HTMLDivElement>(null);

  // Fetch content for the active tab
  /* eslint-disable react-hooks/set-state-in-effect, react-hooks/exhaustive-deps */
  useEffect(() => {
    if (!activeTab) return;
    let cancelled = false;
    setLoading(true);
    fetch(api(`/api/files?file=${encodeURIComponent(activeTab.path)}`))
      .then(r => r.json())
      .then(data => { if (!cancelled) { setContent(data); setLoading(false); } })
      .catch(() => {
        if (cancelled) return;
        setContent({ name: activeTab.name, path: activeTab.path, size: 0, ext: activeTab.ext, error: 'Failed to load' });
        setLoading(false);
      });
    return () => { cancelled = true; };
  }, [activeTab?.path]);
  /* eslint-enable react-hooks/set-state-in-effect, react-hooks/exhaustive-deps */

  // Close color picker on outside click
  useEffect(() => {
    if (!showColorPicker) return;
    const handler = (e: MouseEvent) => {
      if (colorPickerRef.current && !colorPickerRef.current.contains(e.target as Node)) {
        setShowColorPicker(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [showColorPicker]);

  const selectTab = (idx: number) => {
    if (idx === activeIdx) return;
    onUpdate(pane.id, { viewerActiveTab: idx });
  };

  const closeTab = (idx: number) => {
    const next = tabs.filter((_, i) => i !== idx);
    if (next.length === 0) {
      onClose(pane.id);
      return;
    }
    // If closing the active tab or one before it, shift index
    let newActive = activeIdx;
    if (idx < activeIdx) newActive = activeIdx - 1;
    else if (idx === activeIdx) newActive = Math.min(activeIdx, next.length - 1);
    onUpdate(pane.id, { viewerTabs: next, viewerActiveTab: newActive });
  };

  return (
    <div
      className={cn('flex flex-col border rounded-lg overflow-hidden', isMaximized ? 'h-full !rounded-none' : 'h-full')}
      style={{ borderColor: `${pane.color}60` }}
    >
      {/* Title bar */}
      <div
        className="flex items-center gap-2 px-3 py-1.5 text-xs select-none flex-shrink-0"
        style={{ backgroundColor: `${pane.color}30`, borderBottom: `1px solid ${pane.color}40` }}
      >
        {dragHandleProps && (
          <div {...dragHandleProps} className="flex-shrink-0 cursor-grab active:cursor-grabbing text-zinc-500 hover:text-zinc-300 -ml-1">
            <GripVertical className="w-3.5 h-3.5" />
          </div>
        )}

        <div ref={colorPickerRef} className="relative flex-shrink-0">
          <button
            onClick={() => setShowColorPicker(!showColorPicker)}
            className="w-3 h-3 rounded-full hover:ring-2 hover:ring-zinc-500 ring-offset-1 ring-offset-zinc-900 transition-shadow cursor-pointer"
            style={{ backgroundColor: pane.color }}
            title="Change color"
          />
          {showColorPicker && (
            <div className="absolute z-50 top-full left-0 mt-1.5 p-2 bg-zinc-800 border border-zinc-700 rounded-md shadow-xl">
              <ColorPicker
                value={pane.color}
                onChange={(c) => { onUpdate(pane.id, { color: c }); setShowColorPicker(false); }}
              />
            </div>
          )}
        </div>

        <span className="truncate text-zinc-300 font-medium">{pane.title}</span>

        <span
          className="text-[9px] px-1.5 py-0.5 rounded font-medium"
          style={{ backgroundColor: '#06b6d420', color: '#06b6d4' }}
        >
          Viewer
        </span>

        <div className="flex-1" />

        {onMinimize && (
          <button onClick={() => onMinimize(pane.id)} className="text-zinc-300 hover:text-yellow-400" title="Minimize">
            <Minus className="w-3 h-3" />
          </button>
        )}
        <button onClick={() => onToggleMaximize(pane.id)} className="text-zinc-300 hover:text-white" title={isMaximized ? 'Restore' : 'Maximize'}>
          {isMaximized ? <Minimize2 className="w-3 h-3" /> : <Maximize2 className="w-3 h-3" />}
        </button>
        <button onClick={() => onClose(pane.id)} className="text-zinc-300 hover:text-red-400" title="Close viewer">
          <X className="w-3 h-3" />
        </button>
      </div>

      {/* Tab strip */}
      <div className="flex items-stretch gap-px bg-zinc-900 border-b border-zinc-800/60 overflow-x-auto flex-shrink-0">
        {tabs.length === 0 ? (
          <div className="px-3 py-1.5 text-[10px] text-zinc-600">No files open</div>
        ) : tabs.map((t, i) => {
          const active = i === activeIdx;
          return (
            <div
              key={`${t.path}-${i}`}
              className={cn(
                'group flex items-center gap-1.5 pl-2.5 pr-1 py-1.5 text-[11px] border-r border-zinc-800/60 max-w-[200px] flex-shrink-0 cursor-pointer transition-colors',
                active
                  ? 'bg-zinc-950 text-zinc-200'
                  : 'bg-zinc-900 text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800/50',
              )}
              onClick={() => selectTab(i)}
              title={t.path}
            >
              {tabIcon(t.ext)}
              <span className={cn('truncate font-mono', active ? langClass(t.ext) : '')}>{t.name}</span>
              <button
                onClick={(e) => { e.stopPropagation(); closeTab(i); }}
                className="ml-1 p-0.5 text-zinc-600 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                title="Close tab"
              >
                <X className="w-2.5 h-2.5" />
              </button>
            </div>
          );
        })}
      </div>

      {/* Active file metadata strip */}
      {activeTab && content && !content.error && (
        <div className="flex items-center gap-2 px-3 py-1 border-b border-zinc-800/40 bg-zinc-950 flex-shrink-0 text-[9px] text-zinc-600">
          <span className="font-mono truncate flex-1">{activeTab.path}</span>
          {content.lines ? <span>{content.lines} lines</span> : <span>{formatSize(content.size)}</span>}
          {content.truncated && <span className="text-amber-500">(truncated)</span>}
        </div>
      )}

      {/* Body */}
      <div className="flex-1 overflow-auto bg-[#0a0a0a] min-h-0">
        {loading ? (
          <div className="flex items-center justify-center h-full">
            <Loader2 className="w-4 h-4 animate-spin text-zinc-600" />
          </div>
        ) : content ? (
          <FileViewerContent file={content} />
        ) : (
          <div className="flex items-center justify-center h-full text-xs text-zinc-600">
            Click a file in the explorer to view it here.
          </div>
        )}
      </div>
    </div>
  );
}
