/* 主题管理：black（纯黑）/ snow（简约）/ cream（纸质）
   localStorage 记忆，跨页面（含自动化编辑器 iframe）实时同步。 */
'use strict';

(function () {
    var THEME_KEY = 'macrodriver-theme';
    var VALID = ['black', 'snow', 'cream'];

    function apply(theme) {
        if (VALID.indexOf(theme) < 0) theme = 'black';
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
            saved = (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches)
                ? 'snow' : 'black';
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
