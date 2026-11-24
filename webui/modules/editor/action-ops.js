import { state } from '../core/state.js';
import { parseNumber, pathKey, isLoopAction, isContainerAction, containerKind, cloneAction } from './action-model.js';

export { parseNumber, pathKey, keyToPath, isLoopAction, cloneAction } from './action-model.js';

/** 目标里的容器路径：统一认 `containerPath`，兼容老的 `loopPath` */
export function getTargetContainerPath(target) {
  if (!target) return null;
  if (Array.isArray(target.containerPath)) return target.containerPath;
  if (Array.isArray(target.loopPath)) return target.loopPath;
  return null;
}

export function getActionAndContainerByPath(path) {
  if (!Array.isArray(path) || !path.length) return null;
  let container = state.editorRows;

  for (let depth = 0; depth < path.length; depth += 1) {
    const index = path[depth];
    if (!Array.isArray(container) || index < 0 || index >= container.length) return null;
    if (depth === path.length - 1) {
      return { container, index, action: container[index] };
    }

    const action = container[index];
    if (!isContainerAction(action)) return null;
    container = action[2];
  }

  return null;
}

export function getActionAtPath(path) {
  const found = getActionAndContainerByPath(path);
  return found ? found.action : null;
}

export function getParentInsertTarget(path, offset = 0) {
  if (!Array.isArray(path) || !path.length) {
    return { type: 'root', index: state.editorRows.length };
  }

  const parentPath = path.slice(0, -1);
  const index = path[path.length - 1] + offset;
  if (!parentPath.length) {
    return { type: 'root', index };
  }

  return { type: 'container', containerPath: parentPath, index };
}

export function getContainerForTarget(target) {
  if (!target) return null;
  if (target.type === 'root') return state.editorRows;
  const containerPath = getTargetContainerPath(target);
  if (!containerPath) return null;
  const containerAction = getActionAtPath(containerPath);
  return isContainerAction(containerAction) ? containerAction[2] : null;
}

export function isPathPrefix(prefix, full) {
  if (!Array.isArray(prefix) || !Array.isArray(full) || prefix.length > full.length) return false;
  return prefix.every((part, idx) => part === full[idx]);
}

export function clearExpandedLoopKeysByPrefix(path) {
  const prefix = pathKey(path);
  if (!prefix) return;
  Array.from(state.expandedLoopPathKeys).forEach((key) => {
    if (key === prefix || key.startsWith(`${prefix}.`)) {
      state.expandedLoopPathKeys.delete(key);
    }
  });
}

export function setSelectedLine(path, kind = 'action') {
  state.selectedActionPath = Array.isArray(path) ? [...path] : null;
  state.selectedLineKind = kind;
}

export function insertActionAtTarget(action, target) {
  const container = getContainerForTarget(target);
  if (!Array.isArray(container)) {
    throw new Error('插入目标无效');
  }

  const insertIndex = Math.max(0, Math.min(parseNumber(target.index, container.length), container.length));
  container.splice(insertIndex, 0, cloneAction(action));

  const containerPath = getTargetContainerPath(target);
  if (!containerPath || !containerPath.length) {
    setSelectedLine([insertIndex], 'action');
  } else {
    setSelectedLine([...containerPath, insertIndex], 'action');
    state.expandedLoopPathKeys.add(pathKey(containerPath));
  }
}

/**
 * 一次插多行（「敲一下」这种一块 = 两行的块，或者从宏组库拖一整组进来）。
 * 返回最后一行的路径，方便调用方接着选中它。
 */
export function insertRowsAtTarget(rows, target) {
  const list = (Array.isArray(rows) ? rows : []).filter((row) => Array.isArray(row));
  if (!list.length) return null;

  const container = getContainerForTarget(target);
  if (!Array.isArray(container)) {
    throw new Error('插入目标无效');
  }

  let index = Math.max(0, Math.min(parseNumber(target.index, container.length), container.length));
  list.forEach((row) => {
    container.splice(index, 0, cloneAction(row));
    index += 1;
  });

  const containerPath = getTargetContainerPath(target);
  const lastPath = containerPath && containerPath.length
    ? [...containerPath, index - 1]
    : [index - 1];
  setSelectedLine(lastPath, 'action');
  if (containerPath && containerPath.length) state.expandedLoopPathKeys.add(pathKey(containerPath));
  return lastPath;
}

export function moveActionToTarget(sourcePath, target, showToast) {
  const source = getActionAndContainerByPath(sourcePath);
  if (!source) return false;

  const targetContainerPath = getTargetContainerPath(target);
  if (targetContainerPath
    && isPathPrefix(sourcePath, targetContainerPath)
    && isContainerAction(source.action)) {
    const raw = String(containerKind(source.action));
    showToast(`${raw === 'group' ? '宏组' : '循环'}块只能放在自己的${raw === 'group' ? '宏组' : '循环'}体外`);
    return false;
  }

  const targetContainer = getContainerForTarget(target);
  if (!Array.isArray(targetContainer)) return false;

  let targetIndex = Math.max(0, Math.min(parseNumber(target.index, targetContainer.length), targetContainer.length));
  const movingAction = source.action;

  if (source.container === targetContainer && source.index < targetIndex) {
    targetIndex -= 1;
  }
  if (source.container === targetContainer && source.index === targetIndex) {
    return false;
  }

  source.container.splice(source.index, 1);
  targetContainer.splice(targetIndex, 0, movingAction);

  if (!targetContainerPath || !targetContainerPath.length) {
    setSelectedLine([targetIndex], 'action');
  } else {
    setSelectedLine([...targetContainerPath, targetIndex], 'action');
    state.expandedLoopPathKeys.add(pathKey(targetContainerPath));
  }
  return true;
}

/** 路径比较：按层级逐段比数字，短的在前（就是列表里从上到下的顺序） */
function comparePaths(a, b) {
  const shorter = Math.min(a.length, b.length);
  for (let i = 0; i < shorter; i += 1) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}

/**
 * 多选归一：去重、按列表顺序排好、丢掉「已被祖先包含」的子项
 * （拖一个宏组就不必再把组里的动作也算一遍）。
 */
export function normalizeActionPaths(paths) {
  const list = (Array.isArray(paths) ? paths : [])
    .filter((path) => Array.isArray(path) && path.length)
    .slice()
    .sort(comparePaths);

  const out = [];
  list.forEach((path) => {
    if (out.some((kept) => isPathPrefix(kept, path))) return;
    out.push([...path]);
  });
  return out;
}

/** 找到某个动作数组（容器体）当前在整棵树里的路径；根就是 []，找不到返回 null */
export function findContainerPath(target) {
  if (!Array.isArray(target)) return null;
  if (target === state.editorRows) return [];

  const walk = (rows, base) => {
    const list = Array.isArray(rows) ? rows : [];
    for (let i = 0; i < list.length; i += 1) {
      const row = list[i];
      if (!Array.isArray(row)) continue;
      const cmd = String(row[0] ?? '').toLowerCase();
      if ((cmd === 'group' || cmd === 'loop') && Array.isArray(row[2])) {
        if (row[2] === target) return [...base, i];
        const found = walk(row[2], [...base, i]);
        if (found) return found;
      }
    }
    return null;
  };

  return walk(state.editorRows, []);
}

/**
 * 批量移动：一次把若干动作搬到目标位置，保持它们原来的先后顺序。
 * 支持跨容器、支持同一容器内调整位置；拖父块时自动忽略它内部那些子项。
 */
export function moveActionsToTarget(sourcePaths, target, showToast) {
  const paths = normalizeActionPaths(sourcePaths);
  if (!paths.length) return false;

  const targetContainerPath = getTargetContainerPath(target);
  const targetContainer = getContainerForTarget(target);
  if (!Array.isArray(targetContainer)) return false;

  const sources = [];
  for (const path of paths) {
    const info = getActionAndContainerByPath(path);
    if (!info) continue;

    // 容器块不能搬进它自己里面
    if (targetContainerPath
      && isPathPrefix(path, targetContainerPath)
      && isContainerAction(info.action)) {
      if (typeof showToast === 'function') {
        const kind = String(containerKind(info.action));
        showToast(`${kind === 'group' ? '宏组' : '循环'}块只能放在自己的${kind === 'group' ? '宏组' : '循环'}体外`);
      }
      return false;
    }

    sources.push({ path, container: info.container, index: info.index, action: info.action });
  }
  if (!sources.length) return false;

  const originalTargetIndex = Math.max(
    0,
    Math.min(parseNumber(target.index, targetContainer.length), targetContainer.length),
  );

  // 单条拖到自己身上 / 紧挨着的位置：不算移动，避免白记一步历史
  if (sources.length === 1) {
    const only = sources[0];
    if (only.container === targetContainer) {
      const adjusted = originalTargetIndex - (only.index < originalTargetIndex ? 1 : 0);
      if (adjusted === only.index) return false;
    }
  }

  // 同一容器里、排在插入点前面的源被摘走后，插入点要往前挪
  const removedBefore = sources.filter(
    (item) => item.container === targetContainer && item.index < originalTargetIndex,
  ).length;

  // 先摘下来：同一容器里按索引从大到小删，避免数组下标错位
  const byContainer = new Map();
  sources.forEach((item) => {
    if (!byContainer.has(item.container)) byContainer.set(item.container, []);
    byContainer.get(item.container).push(item);
  });
  byContainer.forEach((list) => {
    [...list]
      .sort((a, b) => b.index - a.index)
      .forEach((item) => item.container.splice(item.index, 1));
  });

  let insertIndex = originalTargetIndex - removedBefore;
  insertIndex = Math.max(0, Math.min(insertIndex, targetContainer.length));

  sources.forEach((item, offset) => {
    targetContainer.splice(insertIndex + offset, 0, item.action);
  });

  const lastIndex = insertIndex + sources.length - 1;
  // 搬完之后目标容器可能在树里换了位置（前面的源被摘走了），按它现在的位置选中 / 展开
  const landedContainerPath = findContainerPath(targetContainer);
  const safeContainerPath = Array.isArray(landedContainerPath) ? landedContainerPath : [];
  const lastPath = safeContainerPath.length
    ? [...safeContainerPath, lastIndex]
    : [lastIndex];
  setSelectedLine(lastPath, 'action');
  if (safeContainerPath.length) {
    state.expandedLoopPathKeys.add(pathKey(safeContainerPath));
  }

  // 整批搬走后不再保持多选，免得后面误拖
  state.selectedActionPaths = [];
  return true;
}

/**
 * 批量删除：一次把选中的动作全删掉（多选之后点一下「删除选中」就该一起走）。
 *
 * 用对象身份摘除、不按下标删 —— 前面的删除让后面的下标错位也不会删错行；
 * 父块（循环 / 宏组）被删时它里面的子项自然跟着走，不用单独再删一遍。
 *
 * 返回 { paths, count, focusPath }：paths 是真正删掉的路径，count 是条数，
 * focusPath 是删完之后建议选中的位置（列表删空了就是 null）。
 */
export function deleteActionPaths(paths) {
  const list = normalizeActionPaths(paths);
  if (!list.length) return { paths: [], count: 0, focusPath: null };

  const targets = [];
  list.forEach((path) => {
    const info = getActionAndContainerByPath(path);
    if (info) targets.push({ path, container: info.container, index: info.index, action: info.action });
  });
  if (!targets.length) return { paths: [], count: 0, focusPath: null };

  const byContainer = new Map();
  targets.forEach((target) => {
    if (!byContainer.has(target.container)) byContainer.set(target.container, new Set());
    byContainer.get(target.container).add(target.action);
  });
  byContainer.forEach((set, container) => {
    for (let i = container.length - 1; i >= 0; i -= 1) {
      if (set.has(container[i])) container.splice(i, 1);
    }
  });

  // 落点：第一条被删动作原来的位置；它所在的容器整个被删掉了就退回列表末尾
  const anchor = targets[0];
  const containerPath = findContainerPath(anchor.container);
  let focusPath = null;
  if (Array.isArray(containerPath)) {
    const index = Math.min(anchor.index, anchor.container.length - 1);
    if (index >= 0) focusPath = containerPath.length ? [...containerPath, index] : [index];
  }
  if (!focusPath && state.editorRows.length) focusPath = [state.editorRows.length - 1];

  return { paths: targets.map((target) => target.path), count: targets.length, focusPath };
}

export function resolveInsertTargetAfterSelection() {  if (!Array.isArray(state.selectedActionPath) || !state.selectedActionPath.length) {
    return { type: 'root', index: state.editorRows.length };
  }

  if (state.selectedLineKind === 'loop_end') {
    return getParentInsertTarget(state.selectedActionPath, 1);
  }

  return getParentInsertTarget(state.selectedActionPath, 1);
}
