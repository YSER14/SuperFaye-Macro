import { apiCall } from './core/api.js';
import { q } from './core/dom.js';

const html = document.documentElement;
const collator = new Intl.Collator('zh-CN', { numeric: true, sensitivity: 'base' });
let fonts = [];
let pending;
let loadFailed = false;

function normalize(text) {
  return text.normalize('NFKC').toLowerCase();
}

function renderFontOptions() {
  const search = q('#uiFontSearch');
  const select = q('#uiFontSelect');
  const hint = q('#uiFontHint');
  const query = normalize(search.value.trim());
  const selected = html.dataset.uiFont || '';
  const selectedFont = fonts.find((font) => normalize(font.name) === normalize(selected));
  const matches = fonts.filter((font) => font.searchText.includes(query));
  const options = [new Option('默认字体', '')];

  for (const font of matches) {
    options.push(new Option(font.displayName, font.name));
  }
  if (selectedFont && !matches.includes(selectedFont)) {
    options.push(new Option(selectedFont.displayName, selectedFont.name));
  } else if (selected && !selectedFont) {
    options.push(new Option(selected, selected));
  }

  select.replaceChildren(...options);
  select.value = selectedFont?.name ?? selected;
  search.disabled = fonts.length === 0;

  if (loadFailed) hint.textContent = '无法读取系统字体，重新打开全局设置可重试';
  else if (pending && !fonts.length) hint.textContent = '正在读取 Windows 字体…';
  else if (!fonts.length) hint.textContent = '选择后立即生效并自动保存';
  else if (query) hint.textContent = matches.length ? `找到 ${matches.length} 种字体` : '没有匹配的字体';
  else hint.textContent = `${fonts.length} 种系统字体 · 自动保存`;
}

export function bindFontSettings() {
  q('#uiFontSearch').addEventListener('input', renderFontOptions);
  new MutationObserver(renderFontOptions).observe(html, { attributeFilter: ['data-ui-font'] });
  renderFontOptions();
}

export async function loadFontSettings() {
  if (pending) return pending;
  loadFailed = false;
  pending = apiCall('list_system_fonts')
    .then((result) => {
      fonts = result.fonts.map(({ name, label }) => ({
        name,
        displayName: name === label ? name : `${label} · ${name}`,
        searchText: normalize(`${label} ${name}`),
      })).sort((left, right) => collator.compare(left.displayName, right.displayName));
    })
    .catch(() => { loadFailed = true; })
    .finally(() => {
      pending = undefined;
      renderFontOptions();
    });
  renderFontOptions();
  return pending;
}
