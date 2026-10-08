'use client';

import { api } from '@/lib/api';

export interface FileContent {
  name: string;
  path: string;
  content?: string;
  lines?: number;
  truncated?: boolean;
  size: number;
  ext: string;
  binary?: boolean;
  image?: boolean;
  pdf?: boolean;
  rawUrl?: string;
  tooLarge?: boolean;
  error?: string;
}

export function langClass(ext: string | null | undefined): string {
  if (!ext) return '';
  const map: Record<string, string> = {
    '.ts': 'text-blue-300', '.tsx': 'text-blue-300',
    '.js': 'text-yellow-300', '.jsx': 'text-yellow-300',
    '.json': 'text-green-300', '.md': 'text-zinc-300',
    '.css': 'text-pink-300', '.scss': 'text-pink-300',
    '.html': 'text-orange-300', '.xml': 'text-orange-300',
    '.py': 'text-emerald-300', '.rb': 'text-red-300',
    '.go': 'text-cyan-300', '.rs': 'text-orange-300',
    '.sh': 'text-green-300', '.bash': 'text-green-300',
    '.yml': 'text-purple-300', '.yaml': 'text-purple-300',
    '.sql': 'text-blue-200', '.graphql': 'text-pink-200',
  };
  return map[ext] || 'text-zinc-300';
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

export function FileViewerContent({ file }: { file: FileContent }) {
  if (file.error) return <div className="p-3 text-xs text-red-400">{file.error}</div>;
  if (file.image && file.rawUrl) return (
    <div className="flex items-center justify-center p-4 min-h-[200px]">
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img src={api(file.rawUrl)} alt={file.name} className="max-w-full max-h-full object-contain rounded" />
    </div>
  );
  if (file.image && file.tooLarge) return <div className="p-3 text-xs text-zinc-500">Image too large ({formatSize(file.size)})</div>;
  if (file.pdf && file.rawUrl) return <iframe src={api(file.rawUrl)} className="w-full h-full border-none" title={file.name} />;
  if (file.binary) return <div className="p-3 text-xs text-zinc-500">Binary file ({file.ext}, {formatSize(file.size)})</div>;
  if (file.content !== undefined) return (
    <pre className="text-[11px] leading-4 font-mono p-0 m-0">
      <table className="border-collapse w-full">
        <tbody>
          {file.content.split('\n').map((line, i) => (
            <tr key={i} className="hover:bg-zinc-800/30">
              <td className="text-right pr-3 pl-3 text-zinc-700 select-none w-[1%] whitespace-nowrap">{i + 1}</td>
              <td className={`pr-3 whitespace-pre ${langClass(file.ext)}`}>{line || ' '}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </pre>
  );
  return null;
}
