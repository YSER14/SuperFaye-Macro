import { state } from '../core/state.js';
import { q, qa, showToast } from '../core/dom.js';
import { apiCall } from '../core/api.js';
import { renderMacroTreeList } from '../macro-tree.js';
import { openImportMacroDialog } from '../macro-import.js';
import {
  ACTION_VIEW_BLOCK,
  ACTION_VIEW_TIMELINE,
  ACTION_VIEW_STORAGE_KEY,
  refreshViewModeButtons,
  readStoredViewMode,
} from './action-xmbc.js';
import { normalizeMacroRows, pathKey, keyToPath, flattenGroups, isContainerAction } from './action-model.js';
import { setSelectedLine, insertActionAtTarget, moveActionsToTarget, resolveInsertTargetAfterSelection, getActionAtPath, clearExpandedLoopKeysByPrefix, deleteActionPaths } from './action-ops.js';
import { fillActionEditorByAction, buildActionFromInputs, ensureActionFormDefaultsForType } from './action-form.js';
import { renderActionList, refreshBatchCommentButton, updateCurrentActionFromEditor, saveGroupToLibraryFromPath, readDraggedPaths, clearMultiSelection } from './action-list-render.js';
import { renderBlockPalette, readPaletteDrag, dropPalettePayload } from './block-palette.js';
import { GROUP_LIB_STORAGE_KEY, invalidateGroupLibrary, hydrateGroupLibrary, reloadGroupLibrary } from './group-lib.js';
import { bindMacroAssistant } from './assistant.js';
import { bindActionHistory, resetHistory, markEditorClean } from './history.js';
import { attachDragAutoScroll } from '../core/drag-autoscroll.js';

export { renderActionList };
export { loadMacroToEditor, renderEditorMacroBrowser, bindEditorActions };

function syncActiveFileIndicator() {
  const el = q('#editorActiveFileName');
  const pill = q('#editorActiveFilePill');
  const path = state.selectedMacro;
  if (el) el.textContent = path || '未选择宏文件';
  if (pill) pill.title = path || '当前未选择宏文件';
  state.syncTopbarHeader?.();
}

function topLevelContainerKeys(rows) {
  const keys = new Set();
  (Array.isArray(rows) ? rows : []).forEach((row, index) => {
    if (isContainerAction(row)) keys.add(String(index));
  });
  return keys;
}

async function loadMacroToEditor(path) {
  const data = await apiCall('read_macro', path);
  state.editorRows = normalizeMacroRows(data.rows);
  state.expandedLoopPathKeys = topLevelContainerKeys(state.editorRows);
  state.inlineEditingPathKey = null;
  state.selectedActionPaths = [];
  state.multiSelectVisibleCount = 0;

  if (state.editorRows.length) {
    setSelectedLine([0], 'action');
    fillActionEditorByAction(state.editorRows[0]);
  } else {
    setSelectedLine(null, 'action');
    ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
  }

  state.selectedMacro = path;
  const selectedInput = q('#selectedMacroPath');
  if (selectedInput) selectedInput.value = path;
  syncActiveFileIndicator();

  const { renderMacroList } = await import('../macro-bind.js');
  renderMacroList();
  renderEditorMacroBrowser();
  resetHistory();
  renderActionList();
  renderBlockPalette();
}

function renderEditorMacroBrowser() {
  const list = q('#editorMacroList');
  renderMacroTreeList({
    container: list,
    tree: state.macroTree,
    viewId: 'editor',
    selectedPath: null,
    openedPath: state.selectedMacro,
    onSelect: (path) => {
      loadMacroToEditor(path).catch((err) => showToast(err.message));
    },
  });
}

function restoreActionViewMode() {
  let mode = ACTION_VIEW_BLOCK;
  try {
    mode = readStoredViewMode(window.localStorage);
  } catch (err) { /* 忽略 */ }
  state.actionViewMode = mode;
}

function applyActionViewMode(mode) {
  state.actionViewMode = mode === ACTION_VIEW_TIMELINE ? ACTION_VIEW_TIMELINE : ACTION_VIEW_BLOCK;
  try {
    window.localStorage.setItem(ACTION_VIEW_STORAGE_KEY, state.actionViewMode);
  } catch (err) { /* 忽略 */ }
  refreshViewModeButtons(state.actionViewMode);
  renderActionList();
}

function bindWorkbenchSubtabs() {
  qa('[data-rail-tab]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const target = btn.dataset.railTab;
      qa('[data-rail-tab]').forEach((b) => b.classList.toggle('active', b === btn));
      qa('[data-rail-pane]').forEach((p) => p.classList.toggle('active', p.dataset.railPane === target));
    });
  });

  qa('[data-tool-tab]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const target = btn.dataset.toolTab;
      qa('[data-tool-tab]').forEach((b) => b.classList.toggle('active', b === btn));
      qa('[data-tool-pane]').forEach((p) => p.classList.toggle('active', p.dataset.toolPane === target));
    });
  });
}

async function saveGroupFromPalette() {
  const isGroup = (path) => {
    const action = Array.isArray(path) ? getActionAtPath(path) : null;
    return Array.isArray(action) && String(action[0] ?? '').toLowerCase() === 'group';
  };

  let path = isGroup(state.selectedActionPath) ? state.selectedActionPath : null;

  if (!path) {
    const findFirstGroup = (rows, base = []) => {
      const list = Array.isArray(rows) ? rows : [];
      for (let i = 0; i < list.length; i += 1) {
        const row = list[i];
        if (!Array.isArray(row)) continue;
        const cmd = String(row[0] ?? '').toLowerCase();
        const here = [...base, i];
        if (cmd === 'group') return here;
        if (cmd === 'loop') {
          const found = findFirstGroup(row[2], here);
          if (found) return found;
        }
      }
      return null;
    };
    path = findFirstGroup(state.editorRows);
  }

  if (!path) {
    showToast('还没有宏组可存：先从块库插一个宏组，或把动作收进宏组');
    return;
  }

  await saveGroupToLibraryFromPath(path);
}

function bindEditorActions() {
  state.onSelectMacroForEditor = loadMacroToEditor;
  state.renderEditorMacroBrowser = renderEditorMacroBrowser;
  state.renderRecordMacroBrowser = async () => {};
  state.renderActionList = renderActionList;
  state.saveGroupFromPalette = saveGroupFromPalette;

  bindWorkbenchSubtabs();
  syncActiveFileIndicator();
  restoreActionViewMode();
  renderActionList();
  renderBlockPalette();

  hydrateGroupLibrary()
    .then(() => renderBlockPalette())
    .catch(() => {});

  bindMacroAssistant();
  bindActionHistory();

  window.addEventListener('macro-assistant-effects', async (event) => {
    if (event.detail.some((effect) => effect.kind === 'group')) {
      await reloadGroupLibrary();
      renderBlockPalette();
    }
    if (event.detail.some((effect) => effect.kind === 'macro')) {
      const { refreshMacros } = await import('../macro-bind.js');
      await refreshMacros();
    }
  });

  window.addEventListener('storage', (e) => {
    if (e.key && e.key !== GROUP_LIB_STORAGE_KEY) return;
    invalidateGroupLibrary();
    renderBlockPalette();
  });

  qa('#actionViewSwitch [data-view-mode]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const next = btn.dataset.viewMode === ACTION_VIEW_TIMELINE ? ACTION_VIEW_TIMELINE : ACTION_VIEW_BLOCK;
      if (next === state.actionViewMode) return;
      applyActionViewMode(next);
      showToast(next === ACTION_VIEW_TIMELINE
        ? '已切到时间轴视图（只改显示，不改宏文件）'
        : '已切回积木块视图');
    });
  });

  q('#btnCopyCode')?.addEventListener('click', async () => {
    const text = q('#timelineCode')?.textContent || '';
    if (!text) return showToast('当前宏是空的');
    try {
      await navigator.clipboard.writeText(text);
      showToast('宏串已复制');
    } catch (err) {
      showToast('复制失败，可手动选中文本复制');
    }
  });

  q('#btnRefreshEditorMacroBrowser')?.addEventListener('click', async () => {
    const { refreshMacros } = await import('../macro-bind.js');
    await refreshMacros();
    showToast('编辑区宏文件夹已刷新');
  });

  q('#btnEditorImportText')?.addEventListener('click', importRowsIntoEditor);

  q('#btnBatchComment')?.addEventListener('click', () => {
    state.batchCommentMode = !state.batchCommentMode;
    if (state.batchCommentMode) {
      state.inlineEditingPathKey = null;
      showToast('批量注释模式已开启：点击动作注释或恢复');
    } else {
      showToast('批量注释模式已关闭');
    }
    refreshBatchCommentButton();
    renderActionList();
  });

  q('#btnEditorCreate')?.addEventListener('click', async () => {
    const folder = q('#editorFolder').value.trim() || '宏/录制宏';
    const fileName = q('#editorNewName').value.trim() || '新建宏.json';
    const ret = await apiCall('create_macro', folder, fileName);
    state.selectedMacro = ret.path;
    state.editorRows = [];
    state.expandedLoopPathKeys = new Set();
    state.inlineEditingPathKey = null;
    state.selectedActionPaths = [];
    state.multiSelectVisibleCount = 0;
    setSelectedLine(null, 'action');
    ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
    syncActiveFileIndicator();
    resetHistory();
    renderActionList();
    const { refreshMacros } = await import('../macro-bind.js');
    await refreshMacros();
    showToast(`已创建：${ret.path}`);
  });

  q('#btnEditorRename')?.addEventListener('click', async () => {
    if (!state.selectedMacro) return showToast('请先选择一个宏');
    const newName = q('#editorRenameName').value.trim();
    if (!newName) return showToast('请输入新文件名');

    const ret = await apiCall('rename_macro', state.selectedMacro, newName);
    state.selectedMacro = ret.path;
    const selectedInput = q('#selectedMacroPath');
    if (selectedInput) selectedInput.value = ret.path;
    syncActiveFileIndicator();
    showToast('重命名完成');
    const { refreshMacros, refreshBindings } = await import('../macro-bind.js');
    await refreshMacros();
    await refreshBindings();
  });

  q('#actionType')?.addEventListener('change', () => {
    ensureActionFormDefaultsForType(q('#actionType').value, true);
  });

  q('#btnInsertBelow')?.addEventListener('click', () => {
    try {
      const row = buildActionFromInputs();
      const target = resolveInsertTargetAfterSelection();
      insertActionAtTarget(row, target);
      state.inlineEditingPathKey = pathKey(state.selectedActionPath);
      renderActionList();
    } catch (err) {
      showToast(err.message);
    }
  });

  q('#btnAppendAction')?.addEventListener('click', () => {
    try {
      const row = buildActionFromInputs();
      insertActionAtTarget(row, { type: 'root', index: state.editorRows.length });
      state.inlineEditingPathKey = pathKey(state.selectedActionPath);
      renderActionList();
    } catch (err) {
      showToast(err.message);
    }
  });

  q('#btnUpdateAction')?.addEventListener('click', async () => {
    try {
      const updated = await updateCurrentActionFromEditor();
      if (updated !== false) {
        state.inlineEditingPathKey = pathKey(state.selectedActionPath);
      }
    } catch (err) {
      showToast(err.message);
    }
  });

  q('#btnDeleteAction')?.addEventListener('click', () => {
    const paths = state.selectedActionPaths.length
      ? state.selectedActionPaths
      : (Array.isArray(state.selectedActionPath) && state.selectedActionPath.length ? [state.selectedActionPath] : []);

    if (!paths.length) {
      showToast('请先选择一个动作');
      return;
    }

    const result = deleteActionPaths(paths);
    if (!result.count) {
      showToast('选中动作已失效，请重新选择');
      renderActionList();
      return;
    }

    result.paths.forEach((path) => clearExpandedLoopKeysByPrefix(path));
    state.inlineEditingPathKey = null;
    state.selectedActionPaths = [];
    state.multiSelectVisibleCount = 0;

    const nextAction = Array.isArray(result.focusPath) ? getActionAtPath(result.focusPath) : null;
    if (nextAction) {
      setSelectedLine(result.focusPath, 'action');
      fillActionEditorByAction(nextAction);
    } else {
      setSelectedLine(null, 'action');
      ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
    }

    renderActionList();
    showToast(result.count > 1
      ? `一次删除了 ${result.count} 条动作（Ctrl+Z 可撤销）`
      : '已删除 1 条动作（Ctrl+Z 可撤销）');
  });

  q('#btnSaveEditor')?.addEventListener('click', async () => {
    const saveButton = q('#btnSaveEditor');
    const rows = flattenGroups(state.editorRows);
    let path = state.selectedMacro;
    if (!path && !rows.length) return showToast('请先在宏文件里选择文件，或添加动作');
    saveButton.disabled = true;
    try {
      if (!path) {
        const folder = q('#editorFolder').value.trim() || '宏';
        const name = q('#editorNewName').value.trim() || '新建宏.json';
        const created = await apiCall('create_macro', folder, name);
        path = created.path;
        if (!state.selectedMacro) {
          state.selectedMacro = path;
          const selectedInput = q('#selectedMacroPath');
          if (selectedInput) selectedInput.value = path;
          syncActiveFileIndicator();
        }
      }
      const ret = await apiCall('save_macro', path, rows);
      markEditorClean();
      const synced = Number(ret && ret.cache_refreshed) || 0;
      showToast(synced > 0
        ? `宏已保存，并同步了 ${synced} 个触发键缓存：${path}`
        : `宏已保存：${path}`);
      const { refreshMacros } = await import('../macro-bind.js');
      await refreshMacros();
    } catch (err) {
      showToast(err.message);
    } finally {
      saveButton.disabled = false;
    }
  });

  q('#btnReloadEditor')?.addEventListener('click', async () => {
    if (!state.selectedMacro) return showToast('请先选择宏');
    await loadMacroToEditor(state.selectedMacro);
    showToast('已重新加载');
  });

  document.addEventListener('keydown', (event) => {
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
    if (String(event.key || '').toLowerCase() !== 's') return;
    const page = q('.tab-page[data-tab-page="editor"]');
    if (!page || !page.classList.contains('active')) return;
    event.preventDefault();
    q('#btnSaveEditor')?.click();
  });

  const actionList = q('#actionList');
  if (actionList) {
    attachDragAutoScroll(actionList);

    actionList.addEventListener('dragover', (e) => {
      e.preventDefault();
    });

    actionList.addEventListener('drop', (e) => {
      e.preventDefault();

      const payload = readPaletteDrag(e.dataTransfer);
      if (payload) {
        dropPalettePayload(payload, { type: 'root', index: state.editorRows.length });
        return;
      }

      const macroPath = e.dataTransfer.getData('text/macro-path') || e.dataTransfer.getData('text/plain');
      if (macroPath && macroPath.toLowerCase().endsWith('.json')) {
        insertActionAtTarget(['import', macroPath], { type: 'root', index: state.editorRows.length });
        renderActionList();
        showToast(`已追加调用：${macroPath}`);
        return;
      }

      const paths = readDraggedPaths(e.dataTransfer);
      if (paths.length) {
        const moved = moveActionsToTarget(paths, { type: 'root', index: state.editorRows.length }, showToast);
        if (moved) {
          renderActionList();
          if (paths.length > 1) showToast(`已把 ${paths.length} 条动作搬到列表末尾`);
        }
      }

      state.draggedActionPath = null;
      state.draggedActionPaths = [];
    });

    actionList.addEventListener('click', (event) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('li')) return;
      if (!state.selectedActionPaths.length) return;
      clearMultiSelection();
    });
  }

  q('#multiSelectClear')?.addEventListener('click', () => {
    clearMultiSelection();
  });

  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    if (document.querySelector('.sd-backdrop')) return;
    if (state.inlineEditingPathKey) return;
    if (!state.selectedActionPaths.length) return;
    clearMultiSelection();
  });

  document.addEventListener('click', (e) => {
    if (!state.inlineEditingPathKey) return;

    const target = e.target;
    if (!(target instanceof Element)) return;

    const editingRow = q(`#actionList li[data-line-key="${state.inlineEditingPathKey}"][data-line-kind="action"]`);
    if (editingRow && editingRow.contains(target)) return;

    const editingPanel = q(`#actionList li[data-inline-editor-for="${state.inlineEditingPathKey}"]`);
    if (editingPanel && editingPanel.contains(target)) return;

    if (target.closest('#actionType')
      || target.closest('#actionArg1')
      || target.closest('#actionArg2')
      || target.closest('#btnUpdateAction')
      || target.closest('#btnInsertBelow')
      || target.closest('#btnAppendAction')
      || target.closest('#btnDeleteAction')) {
      return;
    }

    const editingPath = keyToPath(state.inlineEditingPathKey);
    if (editingPath && editingPath.length) {
      setSelectedLine(editingPath, 'action');
      updateCurrentActionFromEditor({ silent: true });
    }
    state.inlineEditingPathKey = null;
    renderActionList();
  }, true);
}

function importRowsIntoEditor() {
  openImportMacroDialog({
    title: '导入现成宏 → 动作列表',
    allowXmbcSimplification: true,
    onRows: ({ rows, stats }) => {
      const incoming = normalizeMacroRows(rows);
      if (!incoming.length) return;
      const before = state.editorRows.length;
      state.editorRows = state.editorRows.concat(incoming);
      setSelectedLine([state.editorRows.length - 1], 'action');
      renderActionList();
      const simplification = stats.simplification;
      if (simplification) {
        const { before: original, after } = simplification;
        showToast(original === after
          ? `已导入 ${after} 行，没有可简化的连续重复段`
          : `已简化并导入，动作行 ${original} → ${after}`);
      } else {
        showToast(`已导入 ${incoming.length} 行（${before} → ${state.editorRows.length}）`);
      }
    },
  });
}
