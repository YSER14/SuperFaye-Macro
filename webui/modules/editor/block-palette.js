import { state } from '../core/state.js';
import { q, showToast } from '../core/dom.js';
import { iconEl } from '../core/icons.js';
import { openConfirmDialog } from '../core/dialog.js';
import { isContainerAction, pathKey } from './action-model.js';
import { getActionAtPath, getParentInsertTarget, insertRowsAtTarget } from './action-ops.js';
import { fillActionEditorByAction } from './action-form.js';
import { groupLibraryEntries, rowsFromLibrary, deleteGroupFromLibrary, listGroupFolders, createGroupFolder, deleteGroupFolder, updateGroupInLibrary, hasGroupInLibrary } from './group-lib.js';
import { openEditGroupDialog, openNewFolderDialog, openGroupNameDialog } from './group-editor-dialog.js';

export const BLOCK_CATALOG = [
  {
    group: '时序',
    items: [
      {
        type: 'wait',
        label: '延迟',
        icon: 'clock',
        tone: 'wait',
        hint: '与下一条动作之间的等待',
        make: () => [['wait', 100]],
      },
    ],
  },
  {
    group: '键盘',
    items: [
      {
        type: 'kd',
        label: '按下',
        icon: 'arrowDown',
        tone: 'key',
        hint: '按住不放，必须配一个松开',
        make: () => [['kd', 'w', 50]],
      },
      {
        type: 'ku',
        label: '松开',
        icon: 'arrowUp',
        tone: 'key',
        hint: '松开按键',
        make: () => [['ku', 'w', 50]],
      },
      {
        type: 'tap',
        label: '敲一下',
        icon: 'keyboard',
        tone: 'key',
        hint: '按下再松开（两行一起插入）',
        make: () => [['kd', 'w', 50], ['ku', 'w', 0]],
      },
    ],
  },
  {
    group: '鼠标',
    items: [
      {
        type: 'md',
        label: '鼠标按下',
        icon: 'mouse',
        tone: 'mouse',
        hint: '左键按住不放，可拖视角',
        make: () => [['md', 'left', 50]],
      },
      {
        type: 'mu',
        label: '鼠标松开',
        icon: 'mouse',
        tone: 'mouse',
        hint: '松开鼠标键',
        make: () => [['mu', 'left', 0]],
      },
      {
        type: 'view',
        label: '视角移动',
        icon: 'crosshair',
        tone: 'view',
        hint: '按 dx,dy 转视角，带时长',
        make: () => [['view', [0, 2], 50]],
      },
    ],
  },
  {
    group: '流程',
    items: [
      {
        type: 'loop',
        label: '循环',
        icon: 'repeat',
        tone: 'flow',
        hint: '把里面那组动作重复 N 次',
        make: () => [['loop', 3, []]],
      },
      {
        type: 'group',
        label: '宏组',
        icon: 'layers',
        tone: 'group',
        hint: '打包一组块，存盘时自动摊平',
        make: () => [['group', '新组', []]],
      },
      {
        type: 'import',
        label: '调用子宏',
        icon: 'link',
        tone: 'import',
        hint: '调用另一个宏文件（import 行）',
        make: () => [['import', '宏/录制宏/示例.json']],
      },
      {
        type: 'note',
        label: '注释',
        icon: 'comment',
        tone: 'note',
        hint: '仅用于备注，不影响执行',
        make: () => [['note', '备注']],
      },
    ],
  },
];

export const PALETTE_BLOCK_MIME = 'text/block-type';
export const PALETTE_LIB_MIME = 'text/block-library';

const BLOCK_INDEX = new Map();
BLOCK_CATALOG.forEach((cat) => cat.items.forEach((item) => BLOCK_INDEX.set(item.type, item)));

export function getPaletteBlock(type) {
  return BLOCK_INDEX.get(String(type || '')) || null;
}

function currentListRenderer() {
  return typeof state.renderActionList === 'function' ? state.renderActionList : () => {};
}

export function resolvePaletteInsertTarget() {
  const path = state.selectedActionPath;
  if (!Array.isArray(path) || !path.length) {
    return { type: 'root', index: state.editorRows.length };
  }

  const action = getActionAtPath(path);
  if (!action) {
    return { type: 'root', index: state.editorRows.length };
  }

  if (isContainerAction(action)) {
    return { type: 'container', containerPath: [...path], index: action[2].length };
  }

  return getParentInsertTarget(path, 1);
}

function expandIfContainer(path) {
  if (!Array.isArray(path) || !path.length) return;
  const action = getActionAtPath(path);
  if (isContainerAction(action)) state.expandedLoopPathKeys.add(pathKey(path));
}

export function insertPaletteBlock(type, target) {
  const item = getPaletteBlock(type);
  if (!item) {
    showToast(`不认识的块：${type}`);
    return null;
  }

  const rows = item.make();
  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget(rows, target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }
  if (lastPath) {
    expandIfContainer(lastPath);
    const inserted = getActionAtPath(lastPath);
    if (inserted) fillActionEditorByAction(inserted);
  }
  currentListRenderer()();

  if (type === 'import') {
    const input = q('#actionArg1');
    if (input) {
      input.focus();
      input.select();
    }
    showToast('已插入「调用子宏」：请在底部参数条修改子宏路径');
  }

  return lastPath;
}

function suggestGroupName() {
  const names = new Set();
  const walk = (rows) => {
    (Array.isArray(rows) ? rows : []).forEach((row) => {
      if (!Array.isArray(row)) return;
      const cmd = String(row[0] ?? '').toLowerCase();
      if (cmd === 'group' || cmd === 'loop') {
        if (cmd === 'group') names.add(String(row[1] ?? '').trim());
        walk(row[2]);
      }
    });
  };
  walk(state.editorRows);

  let index = 1;
  while (names.has(`宏组${index}`)) index += 1;
  return `宏组${index}`;
}

async function insertNamedGroupBlock(target) {
  const payload = await openGroupNameDialog({ name: suggestGroupName() });
  if (!payload) return null;

  const rows = [['group', payload.name, []]];
  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget(rows, target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }

  if (lastPath) {
    expandIfContainer(lastPath);
    const inserted = getActionAtPath(lastPath);
    if (inserted) fillActionEditorByAction(inserted);
  }
  currentListRenderer()();
  showToast(`已插入宏组「${payload.name}」`);
  return lastPath;
}

export function insertLibraryGroup(name, groupPath = '', target) {
  const groupRow = rowsFromLibrary(name, groupPath);
  if (!groupRow) {
    showToast('这条宏组已经不在库里了');
    renderBlockPalette();
    return null;
  }

  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget([groupRow], target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }
  expandIfContainer(lastPath);
  const inserted = lastPath ? getActionAtPath(lastPath) : null;
  if (inserted) fillActionEditorByAction(inserted);
  currentListRenderer()();
  showToast(`已插入宏组「${groupRow[1]}」（${groupRow[2].length} 条动作）`);
  return lastPath;
}

function makePaletteBlock(item) {
  const el = document.createElement('div');
  el.className = `palette-block tone-${item.tone || 'default'}`;
  el.draggable = true;
  el.dataset.blockType = item.type;
  el.setAttribute('role', 'button');
  el.tabIndex = 0;
  el.title = `${item.label} — ${item.hint}\n拖动到右侧列表任意位置，或单击直接插入`;

  const icon = document.createElement('span');
  icon.className = 'pb-icon';
  icon.appendChild(iconEl(item.icon, 15));

  const text = document.createElement('span');
  text.className = 'pb-text';
  const name = document.createElement('b');
  name.textContent = item.label;
  const hint = document.createElement('small');
  hint.textContent = item.hint;
  text.append(name, hint);

  el.append(icon, text);

  el.addEventListener('click', () => {
    if (item.type === 'group') {
      void insertNamedGroupBlock();
      return;
    }
    insertPaletteBlock(item.type);
  });

  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      if (item.type === 'group') {
        void insertNamedGroupBlock();
        return;
      }
      insertPaletteBlock(item.type);
    }
  });

  el.addEventListener('dragstart', (e) => {
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData(PALETTE_BLOCK_MIME, item.type);
    e.dataTransfer.setData('text/plain', `block:${item.type}`);
    el.classList.add('dragging');
  });

  el.addEventListener('dragend', () => el.classList.remove('dragging'));

  return el;
}

function makeLibraryBlock(entry) {
  const el = document.createElement('div');
  el.className = `palette-block palette-lib${entry.nested ? ' nested' : ''}`;
  el.draggable = true;
  el.dataset.libName = entry.name;
  el.dataset.libGroup = entry.groupPath || '';
  el.setAttribute('role', 'button');
  el.tabIndex = 0;
  el.title = entry.nested
    ? `只取「${entry.name}」里的宏组「${entry.groupPath}」：${entry.count} 条动作`
    : `宏组「${entry.name}」：${entry.count} 条动作`;

  const icon = document.createElement('span');
  icon.className = 'pb-icon';
  icon.appendChild(iconEl(entry.nested ? 'chevRight' : 'layers', 14));

  const text = document.createElement('span');
  text.className = 'pb-text';
  const name = document.createElement('b');
  name.textContent = entry.label;
  const hint = document.createElement('small');
  hint.textContent = entry.nested
    ? `来自「${entry.name}」· ${entry.count} 块`
    : (entry.folder ? `${entry.count} 块 · ${entry.folder}` : `${entry.count} 块 · 未分类`);
  text.append(name, hint);

  const edit = document.createElement('button');
  edit.type = 'button';
  edit.className = 'pb-del pb-edit';
  edit.appendChild(iconEl('edit', 11));
  edit.title = entry.nested ? '只能在库里改整条宏组' : `改名 / 换文件夹：${entry.name}`;
  edit.hidden = Boolean(entry.nested);
  edit.addEventListener('click', async (e) => {
    e.stopPropagation();
    if (entry.nested) return;

    const payload = await openEditGroupDialog({
      name: entry.name,
      folder: entry.folder,
      folders: listGroupFolders(),
      taken: (name) => hasGroupInLibrary(name) && name !== entry.name,
    });
    if (!payload) return;

    const ret = updateGroupInLibrary(entry.name, { name: payload.name, folder: payload.folder });
    if (!ret.ok) {
      showToast(ret.reason === 'conflict' ? `库里已经有「${payload.name}」了` : '这条宏组已经不在了');
      renderBlockPalette();
      return;
    }
    renderBlockPalette();
    showToast(ret.folder
      ? `已更新宏组「${ret.name}」→ 文件夹「${ret.folder}」`
      : `已更新宏组「${ret.name}」→ 未分类`);
  });

  const del = document.createElement('button');
  del.type = 'button';
  del.className = 'pb-del';
  del.appendChild(iconEl('x', 11));
  del.title = entry.nested ? '只能在库里删整条宏组' : `从宏组库删掉「${entry.name}」`;
  del.hidden = Boolean(entry.nested);
  del.addEventListener('click', async (e) => {
    e.stopPropagation();
    if (entry.nested) return;
    const ok = await openConfirmDialog({
      title: `删除宏组「${entry.name}」`,
      description: '只删除宏组库中的模板副本，已经插进宏文件里的动作不受影响。',
      confirmText: '从库中删除',
      danger: true,
    });
    if (!ok) return;
    if (!deleteGroupFromLibrary(entry.name)) {
      showToast('这条宏组已经不在库里了');
    } else {
      showToast(`已从宏组库删除「${entry.name}」`);
    }
    renderBlockPalette();
  });

  el.append(icon, text, edit, del);

  const insert = () => insertLibraryGroup(entry.name, entry.groupPath);

  el.addEventListener('click', insert);
  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      insert();
    }
  });
  el.addEventListener('dragstart', (e) => {
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData(PALETTE_LIB_MIME, JSON.stringify({ name: entry.name, group: entry.groupPath || '' }));
    e.dataTransfer.setData('text/plain', `group:${entry.name}`);
    el.classList.add('dragging');
  });
  el.addEventListener('dragend', () => el.classList.remove('dragging'));

  return el;
}

function makeFolderSection(folder, entries) {
  const wrap = document.createElement('div');
  wrap.className = `palette-folder${folder ? '' : ' palette-folder-unfiled'}`;

  const head = document.createElement('div');
  head.className = 'palette-folder-head';

  const collapsed = Boolean(folder) && state.groupFoldersCollapsed.has(folder);

  if (folder) {
    const toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.className = 'palette-folder-toggle';
    toggle.appendChild(iconEl(collapsed ? 'chevRight' : 'chevDown', 11));
    toggle.title = collapsed ? '展开这个文件夹' : '收起这个文件夹';
    toggle.addEventListener('click', (e) => {
      e.stopPropagation();
      if (collapsed) state.groupFoldersCollapsed.delete(folder);
      else state.groupFoldersCollapsed.add(folder);
      renderBlockPalette();
    });
    head.appendChild(toggle);
  }

  const nameEl = document.createElement('span');
  nameEl.className = 'palette-folder-name';
  nameEl.textContent = folder || '未分类';
  head.appendChild(nameEl);

  const countEl = document.createElement('span');
  countEl.className = 'palette-folder-count';
  countEl.textContent = String(entries.filter((entry) => !entry.nested).length);
  head.appendChild(countEl);

  if (folder) {
    const del = document.createElement('button');
    del.type = 'button';
    del.className = 'palette-folder-del';
    del.appendChild(iconEl('x', 11));
    del.title = '删掉文件夹（里面的宏组退回未分类）';
    del.addEventListener('click', async (e) => {
      e.stopPropagation();
      const ok = await openConfirmDialog({
        title: `删除文件夹「${folder}」`,
        description: '里面的宏组不会被删除，会自动退回「未分类」。',
        confirmText: '删除文件夹',
        danger: true,
      });
      if (!ok) return;
      const ret = deleteGroupFolder(folder);
      state.groupFoldersCollapsed.delete(folder);
      renderBlockPalette();
      if (!ret.ok) {
        showToast(`文件夹「${folder}」已经没了`);
        return;
      }
      showToast(ret.moved
        ? `已删掉文件夹「${folder}」，${ret.moved} 个宏组退回未分类`
        : `已删掉文件夹「${folder}」`);
    });
    head.appendChild(del);
  }

  wrap.appendChild(head);

  if (collapsed) return wrap;

  if (!entries.length) {
    const empty = document.createElement('div');
    empty.className = 'palette-folder-empty';
    empty.textContent = folder ? '空文件夹' : '还没有宏组';
    wrap.appendChild(empty);
    return wrap;
  }

  entries.forEach((entry) => wrap.appendChild(makeLibraryBlock(entry)));
  return wrap;
}

export function renderBlockPalette() {
  const host = q('#blockPalette');
  if (!host) return;

  host.textContent = '';

  let staticCount = 0;
  BLOCK_CATALOG.forEach((cat) => {
    const title = document.createElement('div');
    title.className = 'palette-cat';
    title.textContent = cat.group;
    host.appendChild(title);

    cat.items.forEach((item) => {
      staticCount += 1;
      host.appendChild(makePaletteBlock(item));
    });
  });

  const entries = groupLibraryEntries();
  const folders = listGroupFolders();

  const libTitle = document.createElement('div');
  libTitle.className = 'palette-cat palette-cat-lib';

  const libLabel = document.createElement('span');
  libLabel.className = 'palette-cat-label';
  libLabel.textContent = '我的宏组';

  const libTools = document.createElement('span');
  libTools.className = 'palette-cat-tools';

  const saveGroupBtn = document.createElement('button');
  saveGroupBtn.type = 'button';
  saveGroupBtn.className = 'palette-mini-btn';
  saveGroupBtn.textContent = '存组';
  saveGroupBtn.title = '把当前选中的宏组（没选就取第一个）存进库';
  saveGroupBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    if (typeof state.saveGroupFromPalette === 'function') {
      state.saveGroupFromPalette();
    } else {
      showToast('页面还没准备好，稍等一下再点');
    }
  });
  libTools.appendChild(saveGroupBtn);

  const addFolder = document.createElement('button');
  addFolder.type = 'button';
  addFolder.className = 'palette-mini-btn';
  addFolder.textContent = '＋ 文件夹';
  addFolder.title = '新建宏组分类文件夹';
  addFolder.addEventListener('click', async (e) => {
    e.stopPropagation();
    const payload = await openNewFolderDialog({ folders: listGroupFolders() });
    if (!payload) return;
    const ret = createGroupFolder(payload.folder);
    renderBlockPalette();
    showToast(ret.created ? `已新建文件夹「${ret.folder}」` : `文件夹「${ret.folder}」已经有了`);
  });
  libTools.appendChild(addFolder);

  libTitle.append(libLabel, libTools);
  host.appendChild(libTitle);

  if (!entries.length && !folders.length) {
    const empty = document.createElement('div');
    empty.className = 'palette-empty';
    empty.textContent = '宏组库是空的。选中宏组后点「存组」即可保存复用。';
    host.appendChild(empty);
  } else {
    const byFolder = new Map();
    folders.forEach((folder) => byFolder.set(folder, []));
    byFolder.set('', []);

    entries.forEach((entry) => {
      const key = entry.folder || '';
      if (!byFolder.has(key)) byFolder.set(key, []);
      byFolder.get(key).push(entry);
    });

    folders.forEach((folder) => {
      host.appendChild(makeFolderSection(folder, byFolder.get(folder) || []));
    });

    const unfiled = byFolder.get('') || [];
    if (unfiled.length || !folders.length) {
      host.appendChild(makeFolderSection('', unfiled));
    }
  }

  const count = q('#paletteCount');
  if (count) count.textContent = String(staticCount + entries.length);
}

export function readPaletteDrag(dataTransfer) {
  if (!dataTransfer) return null;

  const libRaw = dataTransfer.getData(PALETTE_LIB_MIME);
  if (libRaw) {
    try {
      const parsed = JSON.parse(libRaw);
      if (parsed && parsed.name) return { kind: 'library', name: String(parsed.name), group: String(parsed.group || '') };
    } catch (err) { /* ignore */ }
  }

  const type = dataTransfer.getData(PALETTE_BLOCK_MIME);
  if (type && getPaletteBlock(type)) return { kind: 'block', type };

  return null;
}

export function dropPalettePayload(payload, target) {
  if (!payload) return false;
  if (payload.kind === 'library') {
    insertLibraryGroup(payload.name, payload.group, target);
    return true;
  }
  if (payload.kind === 'block') {
    if (payload.type === 'group') {
      void insertNamedGroupBlock(target);
      return true;
    }
    insertPaletteBlock(payload.type, target);
    return true;
  }
  return false;
}
