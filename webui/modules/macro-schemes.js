import { state } from './core/state.js';
import { q, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';
import { openPromptDialog, openConfirmDialog } from './core/dialog.js';

function schemeList() {
  return Array.isArray(state.schemes) ? state.schemes : [];
}

function findScheme(id) {
  return schemeList().find((item) => item.id === id) || null;
}

function nextSchemeName() {
  const used = new Set(schemeList().map((item) => String(item.name || '')));
  let index = 1;
  while (used.has(`方案 ${index}`)) index += 1;
  return `方案 ${index}`;
}

function activeName() {
  const active = findScheme(state.activeSchemeId);
  return (active && active.name) || state.activeSchemeName || '—';
}

export function renderSchemeTabs() {
  const box = q('#schemeTabs');
  if (!box) return;

  const items = schemeList();
  const fragment = document.createDocumentFragment();

  items.forEach((item) => {
    const tab = document.createElement('button');
    tab.type = 'button';
    tab.className = 'scheme-tab';
    tab.dataset.id = item.id;
    tab.setAttribute('role', 'tab');
    const isActive = item.id === state.activeSchemeId;
    tab.classList.toggle('active', isActive);
    tab.setAttribute('aria-selected', isActive ? 'true' : 'false');

    const count = Number(item.binding_count || 0);
    tab.title = `点击切换方案；双击重命名（${count} 个绑定）`;

    const name = document.createElement('span');
    name.className = 'scheme-tab-name';
    name.textContent = item.name || '未命名';
    tab.appendChild(name);

    if (count > 0) {
      const badge = document.createElement('span');
      badge.className = 'scheme-tab-count';
      badge.textContent = String(count);
      tab.appendChild(badge);
    }

    fragment.appendChild(tab);
  });

  box.innerHTML = '';
  box.appendChild(fragment);

  const hint = q('#schemeHint');
  if (hint) {
    hint.textContent = `共 ${items.length} 个方案 · 当前「${activeName()}」（切换方案会整套换绑并停止正在运行的宏，双击标签可改名）`;
  }
  state.syncTopbarHeader?.();
}

export async function refreshSchemes() {
  const data = await apiCall('list_schemes');
  state.schemes = Array.isArray(data.items) ? data.items : [];
  state.activeSchemeId = data.active || null;
  state.activeSchemeName = data.active_name || '';
  renderSchemeTabs();
  return data;
}

async function refreshBindingView(clearSelection = true) {
  try {
    const mod = await import('./macro-bind.js');
    if (clearSelection && typeof mod.clearBindingSelection === 'function') {
      mod.clearBindingSelection();
    }
    await mod.refreshBindings();
  } catch (err) {
    console.warn('刷新绑定列表失败：', err);
  }
}

async function switchToScheme(id) {
  if (!id || id === state.activeSchemeId) return;
  const ret = await apiCall('switch_scheme', id);
  await refreshSchemes();
  await refreshBindingView(true);
  if (ret.switched) {
    showToast(`已切换到「${ret.active_name}」（${Number(ret.binding_count || 0)} 个绑定）`);
  } else {
    showToast(`已在「${ret.active_name}」`);
  }
}

async function createScheme() {
  const input = await openPromptDialog({
    title: '新建绑定方案',
    description: '新方案从零开始；原方案里的绑定会继续保留，随时可切回。',
    label: '方案名称',
    value: nextSchemeName(),
    placeholder: '例如：深渊连招 / 日常跑图',
    allowEmpty: true,
    confirmText: '创建并切换',
  });
  if (input === null) return;
  const ret = await apiCall('create_scheme', String(input).trim(), false, true);
  await refreshSchemes();
  await refreshBindingView(true);
  showToast(`已新建并切换到「${ret.scheme.name}」`);
}

async function duplicateScheme() {
  const source = state.activeSchemeId;
  if (!source) return;
  const input = await openPromptDialog({
    title: '复制方案',
    description: `将当前方案「${activeName()}」连同全部触发键绑定一并复制。`,
    label: '新方案名称',
    value: nextSchemeName(),
    allowEmpty: true,
    confirmText: '复制并切换',
  });
  if (input === null) return;
  const ret = await apiCall('duplicate_scheme', source, String(input).trim(), true);
  await refreshSchemes();
  await refreshBindingView(true);
  showToast(`已复制为「${ret.scheme.name}」并切换过去`);
}

async function renameScheme(schemeId) {
  const target = schemeId || state.activeSchemeId;
  if (!target) return;
  const current = (findScheme(target) || {}).name || '';
  const input = await openPromptDialog({
    title: '重命名方案',
    label: '新方案名称',
    value: current,
    confirmText: '保存名称',
  });
  if (input === null) return;
  const trimmed = String(input).trim();
  if (!trimmed || trimmed === current) return;
  await apiCall('rename_scheme', target, trimmed);
  await refreshSchemes();
  showToast(`已重命名为「${trimmed}」`);
}

async function deleteScheme() {
  const target = state.activeSchemeId;
  if (!target) return;
  if (schemeList().length <= 1) {
    showToast('至少要保留一个方案');
    return;
  }
  const name = (findScheme(target) || {}).name || '当前方案';
  const confirmed = await openConfirmDialog({
    title: `删除方案「${name}」`,
    description: '删除后该方案下的所有绑定配置将被移除，宏文件本身不受影响。',
    confirmText: '删除方案',
    danger: true,
  });
  if (!confirmed) return;
  const ret = await apiCall('delete_scheme', target);
  await refreshSchemes();
  await refreshBindingView(true);
  showToast(`已删除「${ret.deleted}」，当前「${ret.active_name}」`);
}

export function bindSchemeActions() {
  const tabs = q('#schemeTabs');
  if (tabs) {
    tabs.addEventListener('click', (e) => {
      const tab = e.target.closest('.scheme-tab');
      if (!tab) return;
      switchToScheme(tab.dataset.id).catch((err) => showToast(err.message));
    });
    tabs.addEventListener('dblclick', (e) => {
      const tab = e.target.closest('.scheme-tab');
      if (!tab) return;
      renameScheme(tab.dataset.id).catch((err) => showToast(err.message));
    });
  }

  q('#btnAddScheme')?.addEventListener('click', () => {
    createScheme().catch((err) => showToast(err.message));
  });
  q('#btnDupScheme')?.addEventListener('click', () => {
    duplicateScheme().catch((err) => showToast(err.message));
  });
  q('#btnRenameScheme')?.addEventListener('click', () => {
    renameScheme(state.activeSchemeId).catch((err) => showToast(err.message));
  });
  q('#btnDeleteScheme')?.addEventListener('click', () => {
    deleteScheme().catch((err) => showToast(err.message));
  });
}
