import { state } from './core/state.js';
import { q, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';

// 多方案（绑定组）：对标 XMBC 的「方案 1/2/3…」标签栏，但这里数量不设上限。
// 后端 MacroSchemeApiMixin 负责把当前方案的绑定投影到执行框架的运行时表，
// 前端的活只有两件：画标签、把点击翻成 switch_scheme / create / rename / delete。

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
    tab.title = `点击切换到此方案；双击改名。${count} 个触发键绑定`;

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
    hint.textContent =
      `共 ${items.length} 个方案（不设上限），当前「${activeName()}」。` +
      '一个方案＝一整套触发键绑定；切换方案会整套换绑，正在运行的宏会先停下。双击标签可改名。';
  }
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
    // 列表本身有 900ms 轮询兜底，这里失败不打断操作
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
  const input = window.prompt(
    '新方案的名字（留空用默认名）。\n\n新方案是空的，绑定从零开始；原来的绑定仍然留在当前方案里，随时切回去。',
    nextSchemeName(),
  );
  if (input === null) return; // 点了取消
  const ret = await apiCall('create_scheme', String(input).trim(), false, true);
  await refreshSchemes();
  await refreshBindingView(true);
  showToast(`已新建并切换到「${ret.scheme.name}」`);
}

async function duplicateScheme() {
  const source = state.activeSchemeId;
  if (!source) return;
  const input = window.prompt('复制成哪个名字？（连所有绑定一起复制）', nextSchemeName());
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
  const input = window.prompt('新的方案名', current);
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
    showToast('至少要保留一个方案：可以清空它的绑定，但方案本身要留着');
    return;
  }
  const name = (findScheme(target) || {}).name || '当前方案';
  const confirmed = window.confirm(
    `删除方案「${name}」？\n\n点「确定」立即删除这套绑定（宏文件本身不受影响）；\n点「取消」保留。`,
  );
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
