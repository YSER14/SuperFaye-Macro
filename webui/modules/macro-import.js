import { apiCall } from './core/api.js';
import { showToast } from './core/dom.js';

function parseJsonRows(text) {
  if (!text.startsWith('[')) return null;
  try {
    const parsed = JSON.parse(text);
    return Array.isArray(parsed) && parsed.every((row) => Array.isArray(row)) ? parsed : null;
  } catch {
    return null;
  }
}

function isGhubJsonObject(val) {
  if (!val || typeof val !== 'object') return false;
  if (Array.isArray(val)) return val.length > 0 && val.every((item) => item && typeof item === 'object' && !Array.isArray(item));
  return [
    'macro', 'simpleSequence', 'pressSequence', 'heldSequence',
    'releaseSequence', 'toggleSequence', 'components', 'cards',
    'keyboard', 'mouseMacro', 'mouse', 'delay',
  ].some((k) => Object.prototype.hasOwnProperty.call(val, k));
}

export function detectMacroFormat(rawText) {
  const text = String(rawText || '').trim().replace(/^\uFEFF/, '');
  if (!text) return null;

  if (parseJsonRows(text) !== null) {
    return {
      kind: 'json_rows',
      label: 'JSON 宏行',
      simplifyLabel: '简化 X-Mouse / 雷蛇 / 罗技宏',
      simplifiable: false,
    };
  }

  if (text.startsWith('{') || text.startsWith('[')) {
    try {
      const parsed = JSON.parse(text);
      if (isGhubJsonObject(parsed)) {
        return {
          kind: 'logitech_ghub_json',
          label: '罗技 G HUB JSON',
          simplifyLabel: '简化罗技宏',
          simplifiable: true,
        };
      }
    } catch {
      /* ignore non-JSON */
    }
  }

  if (/<(MacroEvent|MacroEvents|KeyEvent|MouseEvent|MouseMovement)\b/i.test(text)) {
    return {
      kind: 'razer_xml',
      label: '雷蛇 Synapse XML',
      simplifyLabel: '简化雷蛇宏',
      simplifiable: true,
    };
  }

  if (
    /<(multikey|mousebutton)\b/i.test(text)
    || /Logitech\/Cassandra/i.test(text)
    || (/<macro\b/i.test(text) && /<(key|delay)\b/i.test(text))
  ) {
    return {
      kind: 'logitech_xml',
      label: '罗技 LGS XML',
      simplifyLabel: '简化罗技宏',
      simplifiable: true,
    };
  }

  if (
    /\b(OnEvent|EnablePrimaryMouseButtonEvents|PressKey|ReleaseKey|PressAndReleaseKey|PressMouseButton|ReleaseMouseButton|PressAndReleaseMouseButton|MoveMouseRelative|MoveMouseTo|MoveMouseToVirtual|MoveMouseWheel|IsMouseButtonPressed|IsModifierPressed|IsKeyLockOn|Sleep)\s*\(/i.test(text)
    || /\bfunction\s+OnEvent\b/i.test(text)
  ) {
    return {
      kind: 'logitech_lua',
      label: '罗技 Lua 脚本',
      simplifyLabel: '简化罗技宏',
      simplifiable: true,
    };
  }

  return {
    kind: 'xmbc',
    label: 'X-Mouse (XMBC)',
    simplifyLabel: '简化 X-Mouse 宏',
    simplifiable: true,
  };
}

export async function convertMacroText(rawText, simplifyMacro = false) {
  const source = String(rawText || '');
  const text = source.trim();
  if (!text) throw new Error('内容是空的');

  const rowsPayload = parseJsonRows(text);
  const res = simplifyMacro && rowsPayload === null
    ? await apiCall('import_macro_rows', source, null, true, true)
    : await apiCall('import_macro_rows', rowsPayload === null ? text : null, rowsPayload, true);

  const data = res.data || {};
  return {
    rows: Array.isArray(data.rows) ? data.rows : [],
    warnings: Array.isArray(data.warnings) ? data.warnings : [],
    stats: data.stats || {},
  };
}

export function openImportMacroDialog(options) {
  const opts = options || {};
  if (document.getElementById('import-macro-overlay')) return;

  const allowSimplification = Boolean(opts.allowSimplification || opts.allowXmbcSimplification);
  const overlay = document.createElement('div');
  overlay.id = 'import-macro-overlay';
  overlay.className = 'sd-backdrop';

  overlay.innerHTML = `
    <div class="sd-panel sd-panel-lg" role="dialog" aria-modal="true" aria-labelledby="import-macro-title">
      <div class="sd-head sd-head-row">
        <div>
          <h3 id="import-macro-title">${escapeHtml(opts.title || '导入现成宏')}</h3>
          <p class="sd-desc">${escapeHtml(opts.note || '支持 X-Mouse (XMBC)、雷蛇 (Synapse XML)、罗技 (G HUB / LGS Lua、XML、JSON) 或 JSON 宏行。')}</p>
        </div>
        <button class="icon-btn" data-close="1" type="button" aria-label="关闭">×</button>
      </div>
      <div class="sd-body">
        <textarea id="import-macro-text" class="sd-textarea" aria-label="宏文本" spellcheck="false"
          placeholder="{WAITMS:300}{LMB}{WAITMS:120} 或粘贴雷蛇 XML / 罗技 Lua·XML·JSON"></textarea>
        ${allowSimplification ? `
          <div class="import-macro-option">
            <label class="switch">
              <input id="import-macro-simplify" type="checkbox" role="switch"
                     aria-labelledby="import-macro-simplify-label" aria-describedby="import-macro-simplify-description">
              <span class="switch-slider" aria-hidden="true"></span>
            </label>
            <div>
              <label id="import-macro-simplify-label" for="import-macro-simplify">简化 X-Mouse / 雷蛇 / 罗技宏</label>
              <div id="import-macro-simplify-description"></div>
            </div>
          </div>` : ''}
        <div id="import-macro-hint" class="sd-field-hint" role="status"></div>
      </div>
      <div class="sd-foot">
        <button class="sd-btn" data-close="1" type="button">取消</button>
        <button id="import-macro-ok" class="sd-btn sd-btn-primary" type="button">转换</button>
      </div>
    </div>`;

  document.body.appendChild(overlay);

  const close = () => {
    document.removeEventListener('keydown', onKeydown, true);
    overlay.remove();
  };
  const onKeydown = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    }
  };
  document.addEventListener('keydown', onKeydown, true);

  overlay.addEventListener('click', (e) => {
    if (e.target === overlay || e.target.closest('[data-close]')) close();
  });

  const textarea = overlay.querySelector('#import-macro-text');
  const hint = overlay.querySelector('#import-macro-hint');
  const okBtn = overlay.querySelector('#import-macro-ok');
  const simplifyToggle = overlay.querySelector('#import-macro-simplify');
  const simplifyLabel = overlay.querySelector('#import-macro-simplify-label');
  const simplifyDescription = overlay.querySelector('#import-macro-simplify-description');

  const refreshSimplification = () => {
    if (!simplifyToggle) return;
    const detected = detectMacroFormat(textarea.value);
    const isJsonRows = detected?.kind === 'json_rows';
    simplifyToggle.disabled = isJsonRows || okBtn.disabled;
    if (!detected) {
      simplifyLabel.textContent = '简化 X-Mouse / 雷蛇 / 罗技宏';
      simplifyDescription.textContent = '把连续重复的动作收进循环，保留每次按键、鼠标移动和等待。';
    } else if (isJsonRows) {
      simplifyLabel.textContent = '简化 X-Mouse / 雷蛇 / 罗技宏';
      simplifyDescription.textContent = 'JSON 宏行按原格式导入。';
    } else {
      simplifyLabel.textContent = detected.simplifyLabel;
      simplifyDescription.textContent = `已识别：${detected.label} — 把连续重复的动作收进循环，保留每次按键、鼠标移动和等待。`;
    }
    okBtn.textContent = simplifyToggle.checked && !isJsonRows ? '简化并导入' : '转换';
  };

  textarea.addEventListener('input', () => {
    hint.textContent = '';
    refreshSimplification();
  });
  simplifyToggle?.addEventListener('change', () => {
    hint.textContent = '';
    refreshSimplification();
  });
  refreshSimplification();
  textarea.focus();

  okBtn.addEventListener('click', async () => {
    if (!textarea.value.trim()) {
      hint.textContent = '先粘贴一段宏内容进来';
      return;
    }
    const simplifyXmbc = Boolean(simplifyToggle?.checked && !simplifyToggle.disabled);
    okBtn.disabled = true;
    textarea.readOnly = true;
    refreshSimplification();
    hint.textContent = '转换中…';
    try {
      const result = await convertMacroText(textarea.value, simplifyXmbc);
      if (!overlay.isConnected) return;
      if (!result.rows.length) {
        hint.textContent = '这段宏没解析出任何动作';
        return;
      }
      await opts.onRows(result);
      close();
      if (result.warnings.length) {
        console.warn('[导入现成宏] 告警：', result.warnings);
        showToast(`有 ${result.warnings.length} 处没有等价写法，已写成注释行`);
      }
    } catch (err) {
      hint.textContent = err && err.message ? err.message : String(err);
    } finally {
      okBtn.disabled = false;
      textarea.readOnly = false;
      refreshSimplification();
    }
  });
}

function escapeHtml(text) {
  return String(text == null ? '' : text).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));
}
