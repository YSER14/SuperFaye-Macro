// 导入现成宏：把一个文本源（XMBC 宏串 / JSON 宏行）转成动作行。
// 主界面「宏编辑保存」「宏执行绑定」两处共用同一个弹窗与转换逻辑。
import { apiCall } from './core/api.js';
import { showToast } from './core/dom.js';

export async function convertMacroText(rawText) {
  const text = String(rawText || '').trim();
  if (!text) throw new Error('内容是空的');

  // 以 [ 开头就当 JSON 行数组，否则按 XMBC 宏串解析
  let rowsPayload = null;
  if (text.charAt(0) === '[') {
    try {
      const parsed = JSON.parse(text);
      if (Array.isArray(parsed)) rowsPayload = parsed;
    } catch (err) {
      rowsPayload = null;
    }
  }

  const res = await apiCall('import_macro_rows', rowsPayload ? null : text, rowsPayload, true);
  const data = res.data || {};
  return {
    rows: Array.isArray(data.rows) ? data.rows : [],
    warnings: Array.isArray(data.warnings) ? data.warnings : [],
    stats: data.stats || {},
  };
}

/**
 * 打开「导入宏」弹窗。
 * @param {object} options
 * @param {string} options.title      弹窗标题
 * @param {string} [options.note]     标题下的补充说明
 * @param {(result:{rows:Array,warnings:Array,stats:object}) => Promise<void>|void} options.onRows
 */
export function openImportMacroDialog(options) {
  const opts = options || {};
  if (document.getElementById('import-macro-overlay')) return;

  const overlay = document.createElement('div');
  overlay.id = 'import-macro-overlay';
  overlay.style.cssText = 'position:fixed;inset:0;z-index:9000;background:rgba(0,0,0,.62);'
    + 'display:flex;align-items:center;justify-content:center;padding:24px';

  overlay.innerHTML = `
    <div style="width:760px;max-width:94vw;height:440px;max-height:86vh;display:flex;flex-direction:column;
                background:var(--card,#17171b);border:1px solid var(--border,#2a2a30);border-radius:12px;
                overflow:hidden;box-shadow:0 18px 50px rgba(0,0,0,.55)">
      <div style="display:flex;align-items:center;justify-content:space-between;padding:14px 18px;
                  border-bottom:1px solid var(--border,#2a2a30)">
        <strong style="font-size:14px">${escapeHtml(opts.title || '导入现成宏')}</strong>
        <button data-close="1" style="background:none;border:none;color:inherit;font-size:18px;cursor:pointer">×</button>
      </div>
      <div style="flex:1;display:flex;flex-direction:column;gap:10px;padding:16px 18px;overflow:hidden">
        <div style="font-size:12px;line-height:1.7;color:var(--muted,#9aa0a6)">
          粘 <b>XMBC 宏串</b>（<code>{WAITMS:300}{LMB}{CTRL}c</code>）或 <b>JSON 宏行</b>
          （<code>[["kd","w",20]]</code>）。<br>
          ${escapeHtml(opts.note || '旧宏行格式装不下的东西（滚轮、层切换、向窗口发消息…）会写成一条 note 注释行，内容不会丢。')}
        </div>
        <textarea id="import-macro-text" spellcheck="false" placeholder="{WAITMS:300}{LMB}{WAITMS:120}"
          style="flex:1;resize:none;padding:10px;border-radius:8px;border:1px solid var(--border,#2a2a30);
                 background:var(--bg,#0f0f12);color:inherit;font-family:Consolas,monospace;
                 font-size:12.5px;line-height:1.6"></textarea>
        <div id="import-macro-hint" style="font-size:12px;min-height:16px;color:var(--muted,#9aa0a6)"></div>
      </div>
      <div style="display:flex;justify-content:flex-end;gap:10px;padding:12px 18px;
                  border-top:1px solid var(--border,#2a2a30)">
        <button data-close="1" style="padding:7px 14px;border-radius:8px;border:1px solid var(--border,#2a2a30);
                background:transparent;color:inherit;cursor:pointer">取消</button>
        <button id="import-macro-ok" style="padding:7px 16px;border-radius:8px;border:none;background:#e2703a;
                color:#fff;cursor:pointer;font-weight:600">转换</button>
      </div>
    </div>`;

  document.body.appendChild(overlay);

  const close = () => overlay.remove();
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay || e.target.closest('[data-close]')) close();
  });

  const textarea = overlay.querySelector('#import-macro-text');
  const hint = overlay.querySelector('#import-macro-hint');
  const okBtn = overlay.querySelector('#import-macro-ok');
  textarea.focus();

  okBtn.addEventListener('click', async () => {
    if (!textarea.value.trim()) {
      hint.textContent = '先粘一段宏进来';
      return;
    }
    okBtn.disabled = true;
    hint.textContent = '转换中…';
    try {
      const result = await convertMacroText(textarea.value);
      if (!result.rows.length) {
        hint.textContent = '这段宏没解析出任何动作';
        okBtn.disabled = false;
        return;
      }
      await opts.onRows(result);
      close();
      if (result.warnings.length) {
        console.warn('[导入现成宏] 告警：', result.warnings);
        showToast(`有 ${result.warnings.length} 处没有等价写法，已写成 note 注释行`);
      }
    } catch (err) {
      hint.textContent = '转换失败：' + (err && err.message ? err.message : err);
      okBtn.disabled = false;
    }
  });
}

function escapeHtml(text) {
  return String(text == null ? '' : text).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));
}
