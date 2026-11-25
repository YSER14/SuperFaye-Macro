// 统一应用内模态对话框（替代丑陋的原生 window.prompt / window.confirm）

let activeClose = null;

function createButton(text, className, onClick) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = className;
  btn.textContent = text;
  if (onClick) btn.addEventListener('click', onClick);
  return btn;
}

function mountShell({ title, description, confirmText = '确定', cancelText = '取消', danger = false, onCancel }) {
  if (activeClose) activeClose();

  const backdrop = document.createElement('div');
  backdrop.className = 'sd-backdrop';

  const panel = document.createElement('div');
  panel.className = 'sd-panel';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-label', title);

  const head = document.createElement('div');
  head.className = 'sd-head';
  const heading = document.createElement('h3');
  heading.textContent = title;
  head.appendChild(heading);

  if (description) {
    const desc = document.createElement('p');
    desc.className = 'sd-desc';
    desc.textContent = description;
    head.appendChild(desc);
  }

  const body = document.createElement('div');
  body.className = 'sd-body';

  const foot = document.createElement('div');
  foot.className = 'sd-foot';

  let closed = false;
  const close = (result) => {
    if (closed) return;
    closed = true;
    document.removeEventListener('keydown', onKeydown, true);
    backdrop.remove();
    if (activeClose === close) activeClose = null;
    if (result === null && onCancel) onCancel();
  };

  const cancelBtn = createButton(cancelText, 'sd-btn', () => close(null));
  const confirmBtn = createButton(
    confirmText,
    `sd-btn ${danger ? 'sd-btn-danger' : 'sd-btn-primary'}`,
  );
  foot.append(cancelBtn, confirmBtn);

  panel.append(head, body, foot);
  backdrop.appendChild(panel);
  document.body.appendChild(backdrop);

  function onKeydown(event) {
    if (event.key === 'Escape') {
      event.preventDefault();
      close(null);
    }
  }
  document.addEventListener('keydown', onKeydown, true);

  backdrop.addEventListener('mousedown', (event) => {
    if (event.target === backdrop) close(null);
  });

  activeClose = close;
  return { backdrop, panel, body, foot, cancelBtn, confirmBtn, close };
}

/**
 * 文本输入弹窗。确认返回 trimmed 字符串（允许空时依 allowEmpty），取消返回 null。
 */
export function openPromptDialog(options = {}) {
  const {
    title = '输入',
    description = '',
    label = '',
    value = '',
    placeholder = '',
    hint = '',
    confirmText = '确定',
    cancelText = '取消',
    allowEmpty = false,
    validate = null,
  } = options;

  return new Promise((resolve) => {
    const ui = mountShell({
      title,
      description,
      confirmText,
      cancelText,
      onCancel: () => resolve(null),
    });

    const field = document.createElement('label');
    field.className = 'sd-field';
    if (label) {
      const lbl = document.createElement('span');
      lbl.className = 'sd-field-label';
      lbl.textContent = label;
      field.appendChild(lbl);
    }

    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'sd-input';
    input.value = String(value ?? '');
    input.placeholder = placeholder;
    input.autocomplete = 'off';
    input.spellcheck = false;
    field.appendChild(input);

    if (hint) {
      const hintEl = document.createElement('small');
      hintEl.className = 'sd-field-hint';
      hintEl.textContent = hint;
      field.appendChild(hintEl);
    }

    const warn = document.createElement('p');
    warn.className = 'sd-warn';
    warn.hidden = true;

    ui.body.append(field, warn);

    const submit = () => {
      const val = input.value.trim();
      if (!allowEmpty && !val) {
        warn.textContent = '内容不能为空';
        warn.hidden = false;
        input.focus();
        return;
      }
      if (typeof validate === 'function') {
        const msg = validate(val);
        if (msg) {
          warn.textContent = msg;
          warn.hidden = false;
          input.focus();
          return;
        }
      }
      ui.close('ok');
      resolve(val);
    };

    ui.confirmBtn.addEventListener('click', submit);
    input.addEventListener('input', () => {
      warn.hidden = true;
    });
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        submit();
      }
    });

    requestAnimationFrame(() => {
      input.focus();
      input.select();
    });
  });
}

/**
 * 确认弹窗。确认返回 true，取消返回 false。
 */
export function openConfirmDialog(options = {}) {
  const {
    title = '请确认',
    description = '',
    detail = '',
    confirmText = '确定',
    cancelText = '取消',
    danger = false,
  } = options;

  return new Promise((resolve) => {
    const ui = mountShell({
      title,
      description,
      confirmText,
      cancelText,
      danger,
      onCancel: () => resolve(false),
    });

    if (detail) {
      const box = document.createElement('pre');
      box.className = 'sd-detail-box';
      box.textContent = detail;
      ui.body.appendChild(box);
    }

    ui.confirmBtn.addEventListener('click', () => {
      ui.close('ok');
      resolve(true);
    });

    requestAnimationFrame(() => {
      ui.confirmBtn.focus();
    });
  });
}
