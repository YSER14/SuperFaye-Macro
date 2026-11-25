// 全局参数面板 · 人话版渲染器 + 左侧分类导航

import { state } from './core/state.js';
import { q, qa, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';
import { openConfirmDialog } from './core/dialog.js';
import { normalizeParamTextValue } from './core/text.js';
import { enumLabel } from './core/enum-labels.js';
import { RECOMMENDED_RANDOM_DELAY_RULES, HIDE_DEFAULT_FIELD_IDS } from './core/constants.js';
import { PARAM_GROUPS, plainFor } from './core/param-plain.js';

const VIEW_KEY = 'macrodriver-param-view';
const TECH_KEY = 'macrodriver-param-tech';
const ONLY_CHANGED_KEY = 'macrodriver-param-only-changed';

const TIER_ORDER = { basic: 0, advanced: 1, debug: 2 };
const TIER_TEXT = { basic: '常用', advanced: '进阶', debug: '未开放' };
const TYPE_TEXT = {
  bool: '开关',
  int: '整数',
  float: '小数',
  str: '文本',
  list: '列表（JSON 数组）',
  set: '键名集合（JSON 数组）',
  json: 'JSON 对象',
};

const NAV_SHORT_TITLES = {
  keys: '急停与快捷键',
  rhythm: '速度与拟人化',
  precision: '时间精度修正',
  output: '按键发送通道',
  resource: '资源与稳定性',
  record: '录制控制',
  simplify: '轨迹自动简化',
  offline: '离线化简预设',
  editor: '编辑器快捷键',
  device: '驱动设备微调',
  behavior: '其它行为',
  debug: '实验选项',
};

const filters = {
  level: readStored(VIEW_KEY, 'basic', (v) => ['basic', 'advanced', 'all'].includes(v)),
  showTech: readStored(TECH_KEY, '0', (v) => v === '0' || v === '1') === '1',
  onlyChanged: readStored(ONLY_CHANGED_KEY, '0', (v) => v === '0' || v === '1') === '1',
  keyword: '',
};

const rows = new Map();
const navButtons = new Map();
let totalRows = 0;

function readStored(key, fallback, validate) {
  try {
    const raw = localStorage.getItem(key);
    if (raw !== null && (!validate || validate(raw))) return raw;
  } catch (e) { /* 忽略 */ }
  return fallback;
}

function writeStored(key, value) {
  try { localStorage.setItem(key, String(value)); } catch (e) { /* 忽略 */ }
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function formatValue(field, value) {
  if (field.type === 'bool') return value ? '开' : '关';
  if (Array.isArray(field.options) && field.options.length) return enumLabel(value);
  const text = normalizeParamTextValue(value);
  return text === '' ? '（空）' : text;
}

function normalizeForCompare(field, value) {
  if (field.type === 'bool') return value ? '1' : '0';
  if (field.type === 'int' || field.type === 'float') {
    const num = Number(value);
    return Number.isFinite(num) ? String(num) : '';
  }
  if (field.type === 'list' || field.type === 'set' || field.type === 'json') {
    if (Array.isArray(value)) return JSON.stringify(value.map((v) => String(v)));
    if (value && typeof value === 'object') return JSON.stringify(value);
    const text = String(value ?? '').trim();
    if (!text) return field.type === 'json' ? '{}' : '[]';
    try {
      const parsed = JSON.parse(text);
      if (Array.isArray(parsed)) return JSON.stringify(parsed.map((v) => String(v)));
      return JSON.stringify(parsed);
    } catch (e) {
      return text;
    }
  }
  return String(value ?? '').trim();
}

function isFieldChanged(field, input) {
  return normalizeForCompare(field, readTypedInputValue(input))
    !== normalizeForCompare(field, field.default);
}

export async function renderGlobalParams() {
  const container = q('#paramsContainer');
  const nav = q('#paramNav');
  const configPathEl = q('#paramsConfigPath');
  if (!container) return;

  const data = await apiCall('get_global_params');

  rows.clear();
  navButtons.clear();
  totalRows = 0;
  container.innerHTML = '';
  if (nav) nav.innerHTML = '';

  const groups = new Map();
  PARAM_GROUPS.forEach((group) => groups.set(group.id, { ...group, items: [] }));

  let order = 0;
  Object.entries(data.sections || {}).forEach(([section, sectionData]) => {
    const list = Array.isArray(sectionData.rows)
      ? sectionData.rows
      : (Array.isArray(sectionData.items) ? sectionData.items : []);
    list.forEach((field) => {
      const plain = plainFor(field);
      const groupId = groups.has(plain.group) ? plain.group : 'behavior';
      groups.get(groupId).items.push({
        field,
        plain,
        section,
        sectionTitle: sectionData.title || section,
        order: order++,
      });
      totalRows += 1;
    });
  });

  if (configPathEl) {
    configPathEl.textContent = `配置文件：${data.config_path || '未知'}`;
  }

  groups.forEach((group) => {
    if (!group.items.length) return;
    group.items.sort((a, b) => {
      const ta = TIER_ORDER[a.field.tier || 'basic'] ?? 3;
      const tb = TIER_ORDER[b.field.tier || 'basic'] ?? 3;
      if (ta !== tb) return ta - tb;
      return a.order - b.order;
    });
    const card = renderGroup(group);
    container.appendChild(card);

    if (nav) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'param-nav-btn';
      btn.dataset.groupId = group.id;

      const label = el('span', 'param-nav-label', NAV_SHORT_TITLES[group.id] || group.title);
      const badge = el('span', 'param-nav-badge', String(group.items.length));
      btn.append(label, badge);

      btn.addEventListener('click', () => {
        card.open = true;
        qa('.param-nav-btn').forEach((n) => n.classList.toggle('active', n === btn));
        card.scrollIntoView({ behavior: 'smooth', block: 'start' });
      });
      nav.appendChild(btn);
      navButtons.set(group.id, { btn, badge });
    }
  });

  syncToolbar();
  container.classList.toggle('no-tech', !filters.showTech);

  applyParamDependencies();
  refreshRowStates();
  applyFilters();
}

function renderGroup(group) {
  const card = el('details', 'param-group-card');
  card.dataset.groupId = group.id;
  card.open = true;

  const head = el('summary', 'param-group-head');
  head.appendChild(el('span', 'param-group-caret'));
  head.appendChild(el('span', 'param-group-title', group.title));
  const count = el('span', 'param-group-count', `${group.items.length} 项`);
  head.appendChild(count);
  card.appendChild(head);

  if (group.note) card.appendChild(el('p', 'param-group-note', group.note));

  const body = el('div', 'param-group-body');
  group.items.forEach((item) => body.appendChild(renderRow(item)));
  card.appendChild(body);
  return card;
}

function renderRow({ field, plain, section, sectionTitle }) {
  const row = el('article', 'param-row');
  row.dataset.fieldId = field.field_id;
  row.dataset.tier = field.tier || 'basic';
  if (field.type === 'list' || field.type === 'json' || field.type === 'set') {
    row.classList.add('is-wide-control');
  }

  const head = el('div', 'param-row-head');
  const textWrap = el('div', 'param-row-text');

  const titleLine = el('div', 'param-title-line');
  titleLine.appendChild(el('span', 'param-title', plain.title));

  const tech = el('span', 'param-tech', field.label);
  tech.title = `技术名称：${field.label}`;
  titleLine.appendChild(tech);

  const tierTag = el('span', 'param-tier-tag', TIER_TEXT[field.tier || 'basic']);
  tierTag.dataset.tier = field.tier || 'basic';
  if (field.tier && field.tier !== 'basic') titleLine.appendChild(tierTag);

  titleLine.appendChild(el('span', 'param-changed', '已改'));
  if (!HIDE_DEFAULT_FIELD_IDS.has(field.field_id)) {
    const orig = el('span', 'param-orig', `默认 ${formatValue(field, field.default)}`);
    orig.title = '出厂默认值，方便对照';
    titleLine.appendChild(orig);
  }
  textWrap.appendChild(titleLine);

  if (plain.plain) textWrap.appendChild(el('p', 'param-plain', plain.plain));
  if (plain.warn) textWrap.appendChild(el('p', 'param-warn', plain.warn));
  if (field.hint) textWrap.appendChild(el('p', 'param-hint', field.hint));

  const lockNote = el('p', 'param-lock-note');
  lockNote.hidden = true;
  textWrap.appendChild(lockNote);

  const control = el('div', 'param-control');
  const { node: controlNode, input } = buildControl(field, section);
  control.appendChild(controlNode);
  head.appendChild(textWrap);
  head.appendChild(control);
  row.appendChild(head);

  if (field.field_id === 'executor.RANDOM_DELAY_ADJUST_RULES_BY_DELAY_MS') {
    row.appendChild(buildRecommendTools(input));
  }

  const optionHint = el('p', 'param-option-hint');
  row.appendChild(optionHint);

  const foot = el('div', 'param-row-foot');
  foot.appendChild(buildDetails(field, plain, input));
  row.appendChild(foot);

  if (Array.isArray(field.options) && field.options.length && plain.optionHints) {
    updateOptionHint(optionHint, plain, input.value);
  } else {
    optionHint.remove();
  }

  rows.set(field.field_id, {
    field,
    plain,
    section,
    row,
    input,
    lockNote,
    haystack: [
      plain.title,
      plain.plain,
      plain.warn,
      field.label,
      field.key,
      field.field_id,
      field.desc,
      field.hint,
      sectionTitle,
      ...(field.options || []).map((opt) => enumLabel(opt)),
    ].join(' ').toLowerCase(),
  });

  return row;
}

function buildControl(field, section) {
  let input;

  if (field.type === 'bool') {
    const wrap = el('label', 'switch');
    input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = Boolean(field.value);
    assignDataset(input, field, section);
    input.addEventListener('input', onControlChanged);
    input.addEventListener('change', onControlChanged);
    wrap.appendChild(input);
    wrap.appendChild(el('span', 'switch-slider'));
    return { node: wrap, input };
  }

  if (Array.isArray(field.options) && field.options.length) {
    input = document.createElement('select');
    input.className = 'param-input param-select';
    field.options.forEach((opt) => {
      const option = document.createElement('option');
      option.value = String(opt);
      option.textContent = enumLabel(opt);
      option.selected = String(field.value) === String(opt);
      input.appendChild(option);
    });
  } else if (field.type === 'list' || field.type === 'json' || field.type === 'set') {
    input = document.createElement('textarea');
    input.className = 'param-input param-textarea';
    input.rows = 3;
    input.spellcheck = false;
    input.value = normalizeParamTextValue(field.value);
    input.placeholder = field.type === 'json' ? '{"key": "value"}' : '["a", "b"]';
  } else {
    input = document.createElement('input');
    input.className = 'param-input';
    input.type = field.type === 'int' || field.type === 'float' ? 'number' : 'text';
    if (field.type === 'float') input.step = '0.01';
    if (field.type === 'int') input.step = '1';
    input.value = normalizeParamTextValue(field.value);
  }

  assignDataset(input, field, section);
  input.addEventListener('input', onControlChanged);
  input.addEventListener('change', onControlChanged);
  return { node: input, input };
}

function assignDataset(input, field, section) {
  input.dataset.paramSection = section;
  input.dataset.paramKey = field.key;
  input.dataset.paramType = field.type;
  input.dataset.fieldId = field.field_id;
  input.dataset.dependsOn = JSON.stringify(field.depends_on || []);
  input.dataset.paramLabel = field.label;
  input.dataset.currentValue = JSON.stringify(field.value);
}

function buildRecommendTools(input) {
  const tools = el('div', 'param-tools');
  const btn = el('button', 'ghost-mini', '填入推荐分档');
  btn.type = 'button';
  btn.addEventListener('click', () => {
    input.value = JSON.stringify(RECOMMENDED_RANDOM_DELAY_RULES, null, 2);
    onControlChanged();
  });
  tools.appendChild(btn);
  return tools;
}

function buildDetails(field, plain, input) {
  const details = el('details', 'param-more');
  details.appendChild(el('summary', 'param-more-summary', '技术细节'));

  const grid = el('dl', 'param-more-grid');
  const add = (term, valueNode) => {
    if (valueNode === null || valueNode === undefined || valueNode === '') return;
    grid.appendChild(el('dt', null, term));
    const dd = el('dd');
    if (valueNode instanceof Node) dd.appendChild(valueNode);
    else dd.textContent = String(valueNode);
    grid.appendChild(dd);
  };
  const code = (text) => el('code', null, text);

  add('技术名称', field.label);
  add('变量名', code(field.field_id));
  if (!HIDE_DEFAULT_FIELD_IDS.has(field.field_id)) {
    add('默认值', formatValue(field, field.default));
  }
  add('类型', TYPE_TEXT[field.type] || field.type);
  if (field.desc && field.desc !== plain.plain) add('原始说明', field.desc);
  if (field.recommend) add('建议', field.recommend);
  if (field.example) add('示例', code(field.example));
  details.appendChild(grid);

  const actions = el('div', 'param-more-actions');
  const reset = el('button', 'ghost-mini', '填回默认值');
  reset.type = 'button';
  reset.title = '只改界面里的值，保存后才写入配置';
  reset.addEventListener('click', () => {
    fillDefault(field, input);
    onControlChanged();
    showToast(`「${plain.title}」已填回默认值，记得点保存`);
  });
  actions.appendChild(reset);
  details.appendChild(actions);
  return details;
}

function fillDefault(field, input) {
  if (field.type === 'bool') {
    input.checked = Boolean(field.default);
    return;
  }
  if (Array.isArray(field.options) && field.options.length) {
    input.value = String(field.default ?? '');
    return;
  }
  input.value = normalizeParamTextValue(field.default);
}

function updateOptionHint(node, plain, value) {
  const hint = plain.optionHints ? plain.optionHints[String(value)] : '';
  node.textContent = hint ? `当前选择：${hint}` : '';
  node.hidden = !hint;
}

function onControlChanged() {
  applyParamDependencies();
  refreshRowStates();
  if (filters.onlyChanged) applyFilters();
  else syncNavBadges();
}

function readTypedInputValue(input) {
  const type = input.dataset.paramType;
  if (type === 'bool') return input.checked;
  if (type === 'int') return Number.parseInt(input.value || '0', 10) || 0;
  if (type === 'float') return Number(input.value || '0') || 0;
  return input.value;
}

function checkDependency(dep, valuesById) {
  const target = `${dep.section}.${dep.key}`;
  const expected = Object.prototype.hasOwnProperty.call(dep, 'value') ? dep.value : true;
  return String(valuesById[target]) === String(expected);
}

function applyParamDependencies() {
  const inputs = qa('[data-param-key]');
  const valuesById = {};
  inputs.forEach((input) => {
    valuesById[input.dataset.fieldId] = readTypedInputValue(input);
  });

  inputs.forEach((input) => {
    const deps = JSON.parse(input.dataset.dependsOn || '[]');
    let blockedBy = null;
    for (const dep of deps) {
      if (!checkDependency(dep, valuesById)) {
        blockedBy = `${dep.section}.${dep.key}`;
        break;
      }
    }
    const enabled = !blockedBy;
    input.disabled = !enabled;

    const entry = rows.get(input.dataset.fieldId);
    if (!entry) return;
    entry.row.classList.toggle('is-locked', !enabled);

    if (enabled) {
      entry.lockNote.hidden = true;
      entry.lockNote.textContent = '';
    } else {
      const parent = rows.get(blockedBy);
      const parentTitle = parent ? parent.plain.title : blockedBy;
      entry.lockNote.textContent = `需要先打开「${parentTitle}」才生效`;
      entry.lockNote.hidden = false;
    }
  });
}

function refreshRowStates() {
  let changedCount = 0;
  rows.forEach((entry) => {
    const changed = isFieldChanged(entry.field, entry.input);
    entry.row.classList.toggle('is-changed', changed);
    entry.changed = changed;
    if (changed) changedCount += 1;
  });
  return changedCount;
}

function syncNavBadges() {
  let firstVisible = null;
  qa('.param-group-card').forEach((card) => {
    const groupId = card.dataset.groupId;
    const navEntry = navButtons.get(groupId);
    if (!navEntry) return;
    const visibleRows = card.querySelectorAll('.param-row:not(.is-filtered)').length;
    const changedRows = card.querySelectorAll('.param-row:not(.is-filtered).is-changed').length;
    navEntry.btn.hidden = visibleRows === 0;
    navEntry.btn.classList.toggle('has-changed', changedRows > 0);
    navEntry.badge.textContent = String(visibleRows);
    if (!firstVisible && visibleRows > 0) firstVisible = navEntry.btn;
  });
  if (firstVisible && !q('.param-nav-btn.active:not([hidden])')) {
    qa('.param-nav-btn').forEach((n) => n.classList.toggle('active', n === firstVisible));
  }
}

function applyFilters() {
  const keyword = filters.keyword.trim().toLowerCase();
  let shown = 0;

  rows.forEach((entry) => {
    const tier = entry.field.tier || 'basic';
    let visible = true;
    if (filters.level === 'basic') visible = tier === 'basic';
    else if (filters.level === 'advanced') visible = tier !== 'debug';
    if (visible && filters.onlyChanged) visible = Boolean(entry.changed);
    if (visible && keyword) visible = entry.haystack.includes(keyword);
    entry.row.classList.toggle('is-filtered', !visible);
    if (visible) shown += 1;
  });

  qa('.param-group-card').forEach((card) => {
    const visibleRows = card.querySelectorAll('.param-row:not(.is-filtered)').length;
    card.classList.toggle('is-filtered', visibleRows === 0);
    const countEl = card.querySelector('.param-group-count');
    if (countEl) countEl.textContent = `${visibleRows} 项`;
  });

  syncNavBadges();
  updateSummary(shown);
}

function updateSummary(shown) {
  const summary = q('#paramSummary');
  if (!summary) return;
  const changed = refreshRowStates();
  const parts = [`显示 ${shown}/${totalRows}`];
  if (changed) parts.push(`${changed} 项已改`);
  summary.textContent = parts.join(' · ');
  summary.classList.toggle('has-changed', changed > 0);
}

function syncToolbar() {
  const container = q('#paramsContainer');
  if (container) container.classList.toggle('no-tech', !filters.showTech);

  qa('[data-param-tier]').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.paramTier === filters.level);
    btn.setAttribute('aria-pressed', btn.dataset.paramTier === filters.level ? 'true' : 'false');
  });

  const techToggle = q('#chkShowTech');
  if (techToggle) techToggle.checked = filters.showTech;

  const changedToggle = q('#chkOnlyChanged');
  if (changedToggle) changedToggle.checked = filters.onlyChanged;

  const search = q('#paramSearch');
  if (search) search.value = filters.keyword;

  state.showDebugParams = filters.level === 'all';
}

async function saveGlobalParams() {
  const payload = {};

  const parseStructured = (rawText, type) => {
    const text = String(rawText ?? '').trim();
    if (!text) return type === 'json' ? {} : [];

    if (text.startsWith('[') || text.startsWith('{')) {
      const parsed = JSON.parse(text);
      if (type === 'json' && (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed))) {
        throw new Error('这一项需要 JSON 对象');
      }
      if ((type === 'list' || type === 'set') && !Array.isArray(parsed)) {
        throw new Error('这一项需要 JSON 数组');
      }
      return parsed;
    }

    if (type === 'json') {
      throw new Error('这一项请写成 {"key": "value"} 这样的对象');
    }

    return text.split(',').map((s) => s.trim()).filter(Boolean);
  };

  const parseByType = (input) => {
    const type = input.dataset.paramType;
    const currentValue = JSON.parse(input.dataset.currentValue || 'null');
    const text = String(input.value ?? '').trim();

    if (type === 'bool') return input.checked;
    if (type === 'int') {
      if (!text) return Number.parseInt(currentValue, 10) || 0;
      const value = Number.parseInt(text, 10);
      if (!Number.isFinite(value)) throw new Error('需要填整数');
      return value;
    }
    if (type === 'float') {
      if (!text) return Number(currentValue) || 0;
      const value = Number(text);
      if (!Number.isFinite(value)) throw new Error('需要填数字');
      return value;
    }
    if (type === 'list') return parseStructured(text, 'list');
    if (type === 'set') return parseStructured(text, 'set');
    if (type === 'json') return parseStructured(text, 'json');
    return input.value;
  };

  qa('[data-param-key]').forEach((input) => {
    const section = input.dataset.paramSection;
    const key = input.dataset.paramKey;

    if (!payload[section]) payload[section] = {};

    try {
      payload[section][key] = parseByType(input);
    } catch (err) {
      const entry = rows.get(input.dataset.fieldId);
      const label = entry ? entry.plain.title : input.dataset.paramLabel || key;
      throw new Error(`「${label}」填得不对：${err.message}`);
    }
  });

  await apiCall('update_global_params', payload);
  await renderGlobalParams();
  showToast('设置已保存并写入配置文件');

  try {
    if (payload.automation_editor) {
      const cfg = {};
      if (payload.automation_editor.HOTKEY_ENABLED !== undefined) cfg.enabled = payload.automation_editor.HOTKEY_ENABLED;
      if (payload.automation_editor.HOTKEY_RUN !== undefined) cfg.run = payload.automation_editor.HOTKEY_RUN;
      if (payload.automation_editor.HOTKEY_PAUSE !== undefined) cfg.pause = payload.automation_editor.HOTKEY_PAUSE;
      if (payload.automation_editor.HOTKEY_STOP !== undefined) cfg.stop = payload.automation_editor.HOTKEY_STOP;
      if (payload.automation_editor.HOTKEY_FULLSCREEN !== undefined) cfg.fullscreen = payload.automation_editor.HOTKEY_FULLSCREEN;
      if (Object.keys(cfg).length > 0) {
        const existing = JSON.parse(localStorage.getItem('editor_hotkeys') || '{}');
        Object.assign(existing, cfg);
        localStorage.setItem('editor_hotkeys', JSON.stringify(existing));
        if (window._autoEditor && window._autoEditor._loadEditorHotkeys) {
          window._autoEditor._loadEditorHotkeys();
        }
      }
    }
  } catch (e) { /* 忽略 */ }
}

async function importGlobalParams() {
  const selected = await apiCall('choose_param_import_file');
  if (selected.canceled) return;
  const ok = await openConfirmDialog({
    title: '导入设置',
    description: '导入会覆盖当前全部全局设置，确定继续？',
    detail: selected.path,
    confirmText: '确认覆盖导入',
    danger: true,
  });
  if (!ok) return;

  await apiCall('import_global_params', selected.path);
  await renderGlobalParams();
  showToast('设置已导入');
}

async function exportGlobalParams() {
  const selected = await apiCall('choose_param_export_file');
  if (selected.canceled) return;
  const ret = await apiCall('export_global_params', selected.path);
  showToast(`设置已导出：${ret.path}`);
}

async function resetGlobalParamsToDefault() {
  const ok = await openConfirmDialog({
    title: '恢复出厂默认设置',
    description: '所有参数将恢复为默认值并覆盖当前配置文件，操作不可撤销。',
    confirmText: '恢复默认',
    danger: true,
  });
  if (!ok) return;

  await apiCall('reset_global_params_to_default');
  await renderGlobalParams();
  showToast('已恢复默认设置');
}

export function bindParamsActions() {
  const on = (selector, event, handler) => {
    const node = q(selector);
    if (!node) return;
    node.addEventListener(event, handler);
  };

  on('#btnReloadParams', 'click', () => {
    renderGlobalParams()
      .then(() => showToast('设置已刷新'))
      .catch((err) => showToast(err.message));
  });

  on('#btnSaveParamsTop', 'click', () => {
    saveGlobalParams().catch((err) => showToast(err.message));
  });

  on('#btnImportParams', 'click', () => {
    importGlobalParams().catch((err) => showToast(err.message));
  });

  on('#btnExportParams', 'click', () => {
    exportGlobalParams().catch((err) => showToast(err.message));
  });

  on('#btnResetParams', 'click', () => {
    resetGlobalParamsToDefault().catch((err) => showToast(err.message));
  });

  on('#paramSearch', 'input', (event) => {
    filters.keyword = event.target.value || '';
    applyFilters();
  });

  qa('[data-param-tier]').forEach((btn) => {
    btn.addEventListener('click', () => {
      filters.level = btn.dataset.paramTier || 'basic';
      writeStored(VIEW_KEY, filters.level);
      syncToolbar();
      applyFilters();
    });
  });

  on('#chkShowTech', 'change', (event) => {
    filters.showTech = Boolean(event.target.checked);
    writeStored(TECH_KEY, filters.showTech ? '1' : '0');
    const container = q('#paramsContainer');
    if (container) container.classList.toggle('no-tech', !filters.showTech);
  });

  on('#chkOnlyChanged', 'change', (event) => {
    filters.onlyChanged = Boolean(event.target.checked);
    writeStored(ONLY_CHANGED_KEY, filters.onlyChanged ? '1' : '0');
    refreshRowStates();
    applyFilters();
  });
}
