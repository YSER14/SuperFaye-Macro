/* 主题与字体偏好在主控台和自动化编辑器之间共享。 */
'use strict';

(function () {
    var THEME_KEY = 'macrodriver-theme';
    var FONT_KEY = 'macrodriver-ui-font';
    var THEMES = ['black', 'snow', 'cream'];
    var LEGACY_FONTS = new Map([
        ['yahei', 'Microsoft YaHei'], ['dengxian', 'DengXian'], ['simhei', 'SimHei'],
        ['simsun', 'SimSun'], ['kaiti', 'KaiTi'],
    ]);
    var html = document.documentElement;

    function readPreference(key) {
        try { return localStorage.getItem(key); } catch (e) { return null; }
    }

    function savePreference(key, value) {
        try { localStorage.setItem(key, value); } catch (e) { /* 忽略 */ }
    }

    function applyThemeToRoot(theme) {
        if (theme === 'cream') {
            html.removeAttribute('data-theme');
        } else {
            html.setAttribute('data-theme', theme);
        }
    }

    function applyTheme(theme) {
        if (THEMES.indexOf(theme) < 0) theme = 'black';
        html.classList.add('theme-anim');
        applyThemeToRoot(theme);
        document.querySelectorAll('.sw[data-theme-set]').forEach(function (btn) {
            btn.setAttribute('aria-pressed', String(btn.dataset.themeSet === theme));
        });
        window.setTimeout(function () { html.classList.remove('theme-anim'); }, 300);
    }

    function currentTheme() {
        var saved = readPreference(THEME_KEY);
        if (THEMES.indexOf(saved) >= 0) return saved;
        return window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches
            ? 'snow' : 'black';
    }

    function applyFont(preference) {
        var font = LEGACY_FONTS.get(preference);
        if (!font && preference) {
            try { font = JSON.parse(preference); } catch (e) { /* 忽略旧的无效设置 */ }
        }
        if (typeof font === 'string' && font.trim() && !/[\x00-\x1f\x7f]/.test(font)) {
            var family = '"' + font.replace(/["\\]/g, '\\$&') + '"';
            html.style.setProperty('--font-sans', family + ', var(--font-sans-default)');
            html.dataset.uiFont = font;
        } else {
            html.style.removeProperty('--font-sans');
            html.removeAttribute('data-ui-font');
        }
    }

    function init() {
        applyTheme(currentTheme());
        document.querySelectorAll('.sw[data-theme-set]').forEach(function (btn) {
            btn.addEventListener('click', function () {
                var theme = btn.dataset.themeSet;
                savePreference(THEME_KEY, theme);
                applyTheme(theme);
            });
        });

        var fontSelect = document.getElementById('uiFontSelect');
        applyFont(readPreference(FONT_KEY));
        if (fontSelect) {
            fontSelect.addEventListener('change', function () {
                var preference = JSON.stringify(fontSelect.value);
                savePreference(FONT_KEY, preference);
                applyFont(preference);
            });
        }

        window.addEventListener('storage', function (ev) {
            if (ev.key === THEME_KEY && ev.newValue) applyTheme(ev.newValue);
            if (ev.key === FONT_KEY || ev.key === null) applyFont(ev.newValue);
        });
    }

    // 在样式表加载前应用已保存的外观。
    applyThemeToRoot(currentTheme());
    applyFont(readPreference(FONT_KEY));

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
