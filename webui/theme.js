/* 主题管理 —— 移植自 XMBC 拼宏编辑器。
   三主题：cream（默认）/ snow / black；localStorage 记忆，
   跨页面（含自动化编辑器 iframe）storage 事件实时同步。 */
'use strict';

(function () {
    var THEME_KEY = 'macrodriver-theme';
    var VALID = ['cream', 'snow', 'black'];

    function apply(theme) {
        if (VALID.indexOf(theme) < 0) theme = 'cream';
        var html = document.documentElement;
        html.classList.add('theme-anim');
        if (theme === 'cream') {
            html.removeAttribute('data-theme');
        } else {
            html.setAttribute('data-theme', theme);
        }
        document.querySelectorAll('.sw[data-theme-set]').forEach(function (btn) {
            btn.setAttribute('aria-pressed', String(btn.dataset.themeSet === theme));
        });
        window.setTimeout(function () { html.classList.remove('theme-anim'); }, 300);
    }

    function current() {
        var saved = null;
        try { saved = localStorage.getItem(THEME_KEY); } catch (e) { /* 忽略 */ }
        if (VALID.indexOf(saved) < 0) {
            // 首次打开跟随系统明暗：系统深色落纯黑，否则纸质
            saved = (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches)
                ? 'black' : 'cream';
        }
        return saved;
    }

    function init() {
        apply(current());
        document.querySelectorAll('.sw[data-theme-set]').forEach(function (btn) {
            btn.addEventListener('click', function () {
                var theme = btn.dataset.themeSet;
                try { localStorage.setItem(THEME_KEY, theme); } catch (e) { /* 忽略 */ }
                apply(theme);
            });
        });
        // 其他页面（iframe / 主窗口）切主题时跟随
        window.addEventListener('storage', function (ev) {
            if (ev.key === THEME_KEY && ev.newValue) apply(ev.newValue);
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
