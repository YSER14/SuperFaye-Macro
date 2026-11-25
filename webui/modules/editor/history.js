import { state } from '../core/state.js';
import { q, showToast } from '../core/dom.js';
import { cloneAction } from './action-model.js';
import { setSelectedLine, getActionAtPath } from './action-ops.js';
import { fillActionEditorByAction, ensureActionFormDefaultsForType } from './action-form.js';

const MAX_STEPS = 80;

const entries = [];
let cursor = -1;
let applying = false;

function snapshotRows() {
  return cloneAction(Array.isArray(state.editorRows) ? state.editorRows : []);
}

function signature(rows) {
  try {
    return JSON.stringify(rows);
  } catch (err) {
    return `sig-${Date.now()}-${Math.random()}`;
  }
}

function captureState() {
  const rows = snapshotRows();
  return {
    sig: signature(rows),
    rows,
    path: Array.isArray(state.selectedActionPath) ? [...state.selectedActionPath] : null,
    kind: state.selectedLineKind || 'action',
  };
}

function currentRenderer() {
  return typeof state.renderActionList === 'function' ? state.renderActionList : null;
}

function setDisabled(button, disabled) {
  if (!button) return;
  button.disabled = disabled;
}

export function refreshHistoryButtons() {
  const undoBtn = q('#btnUndoAction');
  const redoBtn = q('#btnRedoAction');
  const undoSteps = Math.max(0, cursor);
  const redoSteps = Math.max(0, entries.length - 1 - cursor);

  setDisabled(undoBtn, undoSteps === 0);
  setDisabled(redoBtn, redoSteps === 0);

  if (undoBtn) {
    undoBtn.title = undoSteps
      ? `撤销上一步改动（快捷键 Ctrl+Z，还能退 ${undoSteps} 步）`
      : '没有可撤销的改动了';
  }
  if (redoBtn) {
    redoBtn.title = redoSteps
      ? `重做被撤销的改动（快捷键 Ctrl+Y，还能前进 ${redoSteps} 步）`
      : '没有可重做的改动了';
  }
}

export function resetHistory() {
  entries.length = 0;
  cursor = -1;
  markEditorClean();
  refreshHistoryButtons();
}

export function refreshSaveButtonState() {
  const dirty = Boolean(state.editorDirty);
  const btn = q('#btnSaveEditor');
  if (btn) {
    btn.classList.toggle('unsaved', dirty);
    btn.textContent = dirty ? '保存当前宏 *' : '保存当前宏';
    btn.title = dirty
      ? '动作列表有未保存改动：保存后触发键才会生效'
      : '把动作列表写进当前宏文件（Ctrl+S）';
  }
  const navDot = q('#navEditorDirtyDot');
  if (navDot) navDot.hidden = !dirty;
  const pill = q('#editorActiveFilePill');
  if (pill) pill.classList.toggle('is-dirty', dirty);
}

export function markEditorDirty() {
  if (state.editorDirty) return;
  state.editorDirty = true;
  refreshSaveButtonState();
}

export function markEditorClean() {
  state.editorDirty = false;
  refreshSaveButtonState();
}

export function syncHistoryFromRows() {
  if (applying) return;

  const now = captureState();
  const top = cursor >= 0 ? entries[cursor] : null;
  const isFirstBaseline = entries.length === 0 && cursor < 0;

  if (top && top.sig === now.sig) {
    top.path = now.path;
    top.kind = now.kind;
    refreshHistoryButtons();
    return;
  }

  entries.splice(cursor + 1);
  entries.push(now);
  if (entries.length > MAX_STEPS) entries.shift();
  cursor = entries.length - 1;
  if (!isFirstBaseline) markEditorDirty();
  refreshHistoryButtons();
}

function applyEntry(entry) {
  applying = true;
  try {
    state.editorRows = cloneAction(entry.rows);
    state.inlineEditingPathKey = null;

    let path = null;
    if (Array.isArray(entry.path) && entry.path.length) {
      path = getActionAtPath(entry.path) ? [...entry.path] : null;
    }
    setSelectedLine(path, entry.kind || 'action');

    const action = path ? getActionAtPath(path) : null;
    if (action) fillActionEditorByAction(action);
    else ensureActionFormDefaultsForType(q('#actionType')?.value || 'kd', true);

    const render = currentRenderer();
    if (render) render();
  } finally {
    applying = false;
    refreshHistoryButtons();
  }
}

export function undoAction() {
  if (cursor <= 0) {
    showToast('已经回到最初状态了');
    refreshHistoryButtons();
    return false;
  }
  cursor -= 1;
  applyEntry(entries[cursor]);
  showToast(`已撤销 · 当前 ${entries[cursor].rows.length} 条动作`);
  return true;
}

export function redoAction() {
  if (cursor >= entries.length - 1) {
    showToast('没有可重做的改动了');
    refreshHistoryButtons();
    return false;
  }
  cursor += 1;
  applyEntry(entries[cursor]);
  showToast(`已重做 · 当前 ${entries[cursor].rows.length} 条动作`);
  return true;
}

export function clearActionList() {
  if (!Array.isArray(state.editorRows) || !state.editorRows.length) {
    showToast('动作列表已经是空的');
    return false;
  }

  const count = state.editorRows.length;
  state.editorRows = [];
  state.inlineEditingPathKey = null;
  state.expandedLoopPathKeys = new Set();
  setSelectedLine(null, 'action');
  ensureActionFormDefaultsForType(q('#actionType')?.value || 'kd', true);

  const render = currentRenderer();
  if (render) render();
  else refreshHistoryButtons();

  showToast(`已清空 ${count} 条动作（Ctrl+Z 可撤销）`);
  return true;
}

function isTypingTarget(target) {
  if (!(target instanceof Element)) return false;
  return Boolean(target.closest('input, textarea, select, [contenteditable="true"], [contenteditable=""]'));
}

function editorTabActive() {
  const page = q('.tab-page[data-tab-page="editor"]');
  return Boolean(page && page.classList.contains('active'));
}

export function bindActionHistory() {
  const undoBtn = q('#btnUndoAction');
  const redoBtn = q('#btnRedoAction');
  const clearBtn = q('#btnClearActionList');

  if (undoBtn) undoBtn.addEventListener('click', () => undoAction());
  if (redoBtn) redoBtn.addEventListener('click', () => redoAction());
  if (clearBtn) clearBtn.addEventListener('click', () => clearActionList());

  window.addEventListener('keydown', (event) => {
    if (!(event.ctrlKey || event.metaKey)) return;
    if (event.altKey) return;
    if (isTypingTarget(event.target)) return;
    if (!editorTabActive()) return;

    const key = String(event.key || '').toLowerCase();
    const isUndo = key === 'z' && !event.shiftKey;
    const isRedo = key === 'y' || (key === 'z' && event.shiftKey);
    if (!isUndo && !isRedo) return;

    event.preventDefault();
    if (isUndo) undoAction();
    else redoAction();
  });

  refreshHistoryButtons();
  refreshSaveButtonState();
}
