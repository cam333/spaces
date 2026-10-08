'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Plus, X } from 'lucide-react';
import { api } from '@/lib/api';
import type { Workspace, WorkspaceTodo } from '@/types/claude';

export function WorkspaceTodos({ workspace }: { workspace: Workspace | null }) {
  const workspaceId = workspace?.id ?? null;
  const color = workspace?.color ?? '#6366f1';

  const [todos, setTodos] = useState<WorkspaceTodo[]>([]);
  const [adding, setAdding] = useState(false);
  const [addInput, setAddInput] = useState('');
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editText, setEditText] = useState('');
  const addInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (workspaceId == null) { setTodos([]); return; }
    let cancelled = false;
    fetch(api(`/api/workspaces/${workspaceId}/todos`))
      .then(r => r.json())
      .then((data: WorkspaceTodo[]) => { if (!cancelled) setTodos(Array.isArray(data) ? data : []); })
      .catch(() => { if (!cancelled) setTodos([]); });
    return () => { cancelled = true; };
  }, [workspaceId]);

  const addTodo = useCallback(async () => {
    const text = addInput.trim();
    if (!text || workspaceId == null) { setAddInput(''); setAdding(false); return; }

    const tempId = -Date.now();
    const temp: WorkspaceTodo = {
      id: tempId,
      workspaceId,
      text,
      completed: false,
      sortOrder: Number.MAX_SAFE_INTEGER,
      created: new Date().toISOString(),
      completedAt: null,
    };
    setTodos(prev => [...prev, temp]);
    setAddInput('');
    addInputRef.current?.focus();

    try {
      const res = await fetch(api(`/api/workspaces/${workspaceId}/todos`), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text }),
      });
      if (!res.ok) throw new Error(await res.text());
      const real: WorkspaceTodo = await res.json();
      setTodos(prev => prev.map(t => t.id === tempId ? real : t));
    } catch (err) {
      console.error('failed to add todo', err);
      setTodos(prev => prev.filter(t => t.id !== tempId));
    }
  }, [addInput, workspaceId]);

  const toggleTodo = useCallback(async (todo: WorkspaceTodo) => {
    if (workspaceId == null) return;
    const next = !todo.completed;
    setTodos(prev => prev.map(t => t.id === todo.id
      ? { ...t, completed: next, completedAt: next ? new Date().toISOString() : null }
      : t
    ));
    try {
      const res = await fetch(api(`/api/workspaces/${workspaceId}/todos/${todo.id}`), {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ completed: next }),
      });
      if (!res.ok) throw new Error();
      const real: WorkspaceTodo = await res.json();
      setTodos(prev => prev.map(t => t.id === todo.id ? real : t));
    } catch {
      setTodos(prev => prev.map(t => t.id === todo.id ? todo : t));
    }
  }, [workspaceId]);

  const deleteTodo = useCallback(async (todo: WorkspaceTodo) => {
    if (workspaceId == null) return;
    // Snapshot the original index so we can restore in place on failure,
    // rather than appending to the end.
    let originalIndex = -1;
    setTodos(prev => {
      originalIndex = prev.findIndex(t => t.id === todo.id);
      return prev.filter(t => t.id !== todo.id);
    });
    try {
      const res = await fetch(api(`/api/workspaces/${workspaceId}/todos/${todo.id}`), { method: 'DELETE' });
      if (!res.ok) throw new Error();
    } catch {
      setTodos(prev => {
        if (originalIndex < 0 || prev.some(t => t.id === todo.id)) return prev;
        const next = [...prev];
        next.splice(Math.min(originalIndex, next.length), 0, todo);
        return next;
      });
    }
  }, [workspaceId]);

  const saveEdit = useCallback(async (todo: WorkspaceTodo) => {
    const text = editText.trim();
    setEditingId(null);
    if (!text || text === todo.text || workspaceId == null) return;
    setTodos(prev => prev.map(t => t.id === todo.id ? { ...t, text } : t));
    try {
      const res = await fetch(api(`/api/workspaces/${workspaceId}/todos/${todo.id}`), {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text }),
      });
      if (!res.ok) throw new Error();
    } catch {
      setTodos(prev => prev.map(t => t.id === todo.id ? { ...t, text: todo.text } : t));
    }
  }, [editText, workspaceId]);

  const clearCompleted = useCallback(async () => {
    if (workspaceId == null) return;
    const done = todos.filter(t => t.completed);
    if (done.length === 0) return;
    setTodos(prev => prev.filter(t => !t.completed));
    const results = await Promise.allSettled(
      done.map(t => fetch(api(`/api/workspaces/${workspaceId}/todos/${t.id}`), { method: 'DELETE' }))
    );
    if (results.some(r => r.status === 'rejected' || (r.status === 'fulfilled' && !r.value.ok))) {
      // Reconcile by refetching
      fetch(api(`/api/workspaces/${workspaceId}/todos`))
        .then(r => r.json())
        .then((data: WorkspaceTodo[]) => setTodos(Array.isArray(data) ? data : []))
        .catch(() => {});
    }
  }, [todos, workspaceId]);

  const sorted = [...todos].sort((a, b) => {
    if (a.completed !== b.completed) return a.completed ? 1 : -1;
    if (!a.completed) return a.sortOrder - b.sortOrder;
    const aT = a.completedAt ?? '';
    const bT = b.completedAt ?? '';
    return bT.localeCompare(aT);
  });
  const completedCount = sorted.filter(t => t.completed).length;

  const chipHeight = 'h-7';
  const chipShape = `${chipHeight} rounded-full inline-flex items-center gap-1.5 pl-2 pr-1 py-1 text-xs border whitespace-nowrap`;

  return (
    <div className="border-b border-zinc-800 bg-zinc-900/40 px-4 py-2 flex-shrink-0">
      <div className="flex flex-wrap items-center gap-1.5 max-h-[30vh] overflow-y-auto">
        {sorted.map(todo => {
          const isEditing = editingId === todo.id;
          const activeStyle = {
            backgroundColor: `${color}22`,
            borderColor: `${color}66`,
            color: color,
          };
          const doneStyle = {
            backgroundColor: 'rgba(39, 39, 42, 0.5)',
            borderColor: 'rgba(63, 63, 70, 0.5)',
            color: 'rgb(113, 113, 122)',
          };
          const style = todo.completed ? doneStyle : activeStyle;

          return (
            <div
              key={todo.id}
              className={`group ${chipShape}`}
              style={style}
            >
              <button
                onClick={() => toggleTodo(todo)}
                className="w-3.5 h-3.5 rounded-full border-2 flex items-center justify-center flex-shrink-0 transition-colors"
                style={{
                  borderColor: todo.completed ? color : `${color}aa`,
                  backgroundColor: todo.completed ? color : 'transparent',
                }}
                aria-label={todo.completed ? 'Mark incomplete' : 'Mark complete'}
              >
                {todo.completed && <Check className="w-2.5 h-2.5 text-white" />}
              </button>

              {isEditing ? (
                <input
                  autoFocus
                  value={editText}
                  onChange={e => setEditText(e.target.value)}
                  onBlur={() => saveEdit(todo)}
                  onKeyDown={e => {
                    if (e.key === 'Enter') saveEdit(todo);
                    if (e.key === 'Escape') setEditingId(null);
                  }}
                  className="bg-transparent focus:outline-none max-w-[240px]"
                  style={{ color: 'inherit' }}
                />
              ) : (
                <button
                  onClick={() => { setEditingId(todo.id); setEditText(todo.text); }}
                  className={`text-left truncate max-w-[200px] ${todo.completed ? 'line-through' : ''}`}
                  style={{ color: 'inherit' }}
                >
                  {todo.text}
                </button>
              )}

              <button
                onClick={() => deleteTodo(todo)}
                className="opacity-0 group-hover:opacity-100 transition-opacity rounded-full p-0.5 hover:bg-black/20 flex-shrink-0"
                style={{ color: 'inherit' }}
                aria-label="Delete todo"
              >
                <X className="w-3 h-3" />
              </button>
            </div>
          );
        })}

        {/* Add chip */}
        {adding ? (
          <div className={`${chipShape} border-dashed border-zinc-600 text-zinc-300`}>
            <Plus className="w-3 h-3 text-zinc-500 flex-shrink-0" />
            <input
              ref={addInputRef}
              autoFocus
              value={addInput}
              onChange={e => setAddInput(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter') addTodo();
                if (e.key === 'Escape') { setAddInput(''); setAdding(false); }
              }}
              onBlur={() => {
                if (addInput.trim()) addTodo();
                else setAdding(false);
              }}
              placeholder="todo…"
              className="bg-transparent focus:outline-none text-xs placeholder:text-zinc-600 max-w-[200px]"
            />
          </div>
        ) : (
          <button
            onClick={() => { if (workspaceId != null) setAdding(true); }}
            disabled={workspaceId == null}
            className={`${chipHeight} rounded-full inline-flex items-center gap-1 px-3 py-1 text-xs border border-dashed border-zinc-700 text-zinc-500 hover:text-zinc-300 hover:border-zinc-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap`}
          >
            <Plus className="w-3 h-3" />
            {workspaceId == null ? 'Select a workspace' : 'add todo'}
          </button>
        )}

        {/* Clear completed */}
        {completedCount > 0 && (
          <button
            onClick={clearCompleted}
            className="ml-auto text-[10px] text-zinc-500 hover:text-zinc-300 px-2 py-1 transition-colors whitespace-nowrap"
          >
            clear {completedCount} done
          </button>
        )}
      </div>
    </div>
  );
}
