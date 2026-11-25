// -*- coding: utf-8 -*-
/**
 * 宏组的命名 / 归档对话框。
 *
 * 三个入口：
 *   openSaveGroupDialog  存组进库：起名字 + 选文件夹（可现场新建）
 *   openEditGroupDialog  库里已有条目：改名 + 换文件夹
 *   openNewFolderDialog  只建一个空文件夹
 *
 * 都是 Promise 风格：确认时 resolve 表单值，取消 / Esc / 点遮罩 resolve(null)。
 * 只负责收集输入，写库交给 group-lib.js，调用方负责 toast 和重画左栏。
 */

let activeClose = null;

function closeActiveDialog() {
  if (activeClose) activeClose();
}

function makeButton(text, className, onClick) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = className;
  btn.textContent = text;
  if (onClick) btn.addEventListener('click', onClick);
  return btn;
}

function makeField(labelText, control, hintText) {
  const wrap = document.createElement('label');
  wrap.className = 'sd-field';

  const label = document.createElement('span');
  label.className = 'sd-field-label';
  label.textContent = labelText;

  wrap.append(label, control);

  if (hintText) {
    const hint = document.createElement('small');
    hint.className = 'sd-field-hint';
    hint.textContent = hintText;
    wrap.appendChild(hint);
  }
  return wrap;
}

/**
 * 搭一个模态壳，返回 { body, foot, panel, setSubmitDisabled, close }。
 * 只建一次，内容由调用方往里塞。
 */
function mountDialog({ title, describe, submitText, onCancel }) {
  closeActiveDialog();

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
  if (describe) {
    const desc = document.createElement('p');
    desc.className = 'sd-desc';
    desc.textContent = describe;
    head.appendChild(desc);
  }

  const body = document.createElement('div');
  body.className = 'sd-body';

  const foot = document.createElement('div');
  foot.className = 'sd-foot';

  panel.append(head, body, foot);
  backdrop.appendChild(panel);
  document.body.appendChild(backdrop);

  let closed = false;
  const close = (result) => {
    if (closed) return;
    closed = true;
    document.removeEventListener('keydown', onKeydown, true);
    backdrop.remove();
    if (activeClose === close) activeClose = null;
    if (result === null && onCancel) onCancel();
  };

  const cancelBtn = makeButton('取消', 'sd-btn', () => close(null));
  foot.appendChild(cancelBtn);

  const submitBtn = makeButton(submitText || '保存', 'sd-btn sd-btn-primary');
  foot.appendChild(submitBtn);

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
  requestAnimationFrame(() => {
    const focusTarget = body.querySelector('input, select');
    if (focusTarget) {
      focusTarget.focus();
      if (focusTarget.select && focusTarget.tagName === 'INPUT') focusTarget.select();
    }
  });

  return { backdrop, panel, body, foot, submitBtn, cancelBtn, close };
}

/**
 * 存组对话框。
 * @param {{name?:string, folder?:string, folders?:string[], taken?:(name:string)=>boolean}} options
 * @returns {Promise<{name:string, folder:string, cover:boolean}|null>}
 */
export function openSaveGroupDialog(options = {}) {
  const folders = Array.isArray(options.folders) ? options.folders : [];
  const taken = typeof options.taken === 'function' ? options.taken : () => false;

  return new Promise((resolve) => {
    const ui = mountDialog({
      title: '保存宏组',
      describe: '给这组动作起个名字，再挑个文件夹；左栏「我的宏组」会按文件夹归类。',
      submitText: '保存',
      onCancel: () => resolve(null),
    });

    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'sd-input';
    nameInput.value = String(options.name || '').trim();
    nameInput.placeholder = '例如：开场 / 连招A / 收尾';
    nameInput.maxLength = 60;
    nameInput.autocomplete = 'off';
    nameInput.spellcheck = false;

    const folderSelect = document.createElement('select');
    folderSelect.className = 'sd-input';
    const optUnfiled = document.createElement('option');
    optUnfiled.value = '';
    optUnfiled.textContent = '未分类';
    folderSelect.appendChild(optUnfiled);
    folders.forEach((folder) => {
      const opt = document.createElement('option');
      opt.value = folder;
      opt.textContent = folder;
      folderSelect.appendChild(opt);
    });
    const optNew = document.createElement('option');
    optNew.value = '__new__';
    optNew.textContent = '＋ 新建文件夹…';
    folderSelect.appendChild(optNew);

    const defaultFolder = String(options.folder || '');
    folderSelect.value = folders.includes(defaultFolder) ? defaultFolder : '';

    const newFolderInput = document.createElement('input');
    newFolderInput.type = 'text';
    newFolderInput.className = 'sd-input';
    newFolderInput.placeholder = '新文件夹名，例如：连招/BOSS';
    newFolderInput.maxLength = 60;
    newFolderInput.autocomplete = 'off';
    newFolderInput.spellcheck = false;

    const newFolderField = makeField('新文件夹', newFolderInput, '斜杠可以分层，显示时会保留层级文字。');
    newFolderField.hidden = true;

    const warn = document.createElement('p');
    warn.className = 'sd-warn';
    warn.hidden = true;

    const renameRow = document.createElement('label');
    renameRow.className = 'sd-check';
    const renameBox = document.createElement('input');
    renameBox.type = 'checkbox';
    renameBox.checked = true;
    const renameText = document.createElement('span');
    renameText.textContent = '同时给序列里的宏组改名';
    renameRow.append(renameBox, renameText);

    ui.body.appendChild(makeField('组名', nameInput, '库里同名会直接覆盖——看到提示就说明撞名了。'));
    ui.body.appendChild(makeField('文件夹', folderSelect));
    ui.body.appendChild(newFolderField);
    ui.body.appendChild(renameRow);
    ui.body.appendChild(warn);

    const refresh = () => {
      const name = nameInput.value.trim();
      const conflict = Boolean(name) && taken(name);
      warn.hidden = !conflict;
      warn.textContent = conflict
        ? `库里已经有「${name}」了，保存会覆盖它（位置也会跟着这次选择走）。`
        : '';
      ui.submitBtn.textContent = conflict ? '覆盖保存' : '保存';
      const sameName = name === (options.name || '').trim();
      renameRow.hidden = sameName;
      renameBox.disabled = sameName;
    };

    const syncNewFolder = () => {
      const isNew = folderSelect.value === '__new__';
      newFolderField.hidden = !isNew;
      if (isNew) newFolderInput.focus();
    };

    nameInput.addEventListener('input', refresh);
    folderSelect.addEventListener('change', syncNewFolder);

    const submit = () => {
      const name = nameInput.value.trim();
      if (!name) {
        nameInput.focus();
        warn.hidden = false;
        warn.textContent = '先给宏组起个名字。';
        return;
      }
      let folder = folderSelect.value;
      if (folder === '__new__') {
        folder = newFolderInput.value.trim();
        if (!folder) {
          newFolderInput.focus();
          warn.hidden = false;
          warn.textContent = '新建文件夹的话，把文件夹名填上。';
          return;
        }
      }
      const payload = {
        name,
        folder,
        cover: Boolean(taken(name)),
        renameInList: renameBox.checked && name !== (options.name || '').trim(),
      };
      ui.close('submitted');
      resolve(payload);
    };

    ui.submitBtn.addEventListener('click', submit);
    [nameInput, newFolderInput].forEach((input) => {
      input.addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
          event.preventDefault();
          submit();
        }
      });
    });

    refresh();
  });
}

/**
 * 编辑库里已有宏组：改名 + 换文件夹。
 * @param {{name:string, folder?:string, folders?:string[], taken?:(name:string)=>boolean}} options
 * @returns {Promise<{name:string, folder:string}|null>}
 */
export function openEditGroupDialog(options = {}) {
  const folders = Array.isArray(options.folders) ? options.folders : [];
  const original = String(options.name || '').trim();
  const taken = typeof options.taken === 'function' ? options.taken : () => false;

  return new Promise((resolve) => {
    const ui = mountDialog({
      title: '编辑宏组',
      describe: `正在改库里的「${original}」；改名不会动已经插进宏里的动作。`,
      submitText: '保存修改',
      onCancel: () => resolve(null),
    });

    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'sd-input';
    nameInput.value = original;
    nameInput.maxLength = 60;
    nameInput.autocomplete = 'off';
    nameInput.spellcheck = false;

    const folderSelect = document.createElement('select');
    folderSelect.className = 'sd-input';
    const optUnfiled = document.createElement('option');
    optUnfiled.value = '';
    optUnfiled.textContent = '未分类';
    folderSelect.appendChild(optUnfiled);
    folders.forEach((folder) => {
      const opt = document.createElement('option');
      opt.value = folder;
      opt.textContent = folder;
      folderSelect.appendChild(opt);
    });
    const optNew = document.createElement('option');
    optNew.value = '__new__';
    optNew.textContent = '＋ 新建文件夹…';
    folderSelect.appendChild(optNew);

    const currentFolder = String(options.folder || '');
    folderSelect.value = folders.includes(currentFolder) ? currentFolder : '';

    const newFolderInput = document.createElement('input');
    newFolderInput.type = 'text';
    newFolderInput.className = 'sd-input';
    newFolderInput.placeholder = '新文件夹名';
    newFolderInput.maxLength = 60;
    newFolderInput.autocomplete = 'off';
    newFolderInput.spellcheck = false;

    const newFolderField = makeField('新文件夹', newFolderInput);
    newFolderField.hidden = true;

    const warn = document.createElement('p');
    warn.className = 'sd-warn';
    warn.hidden = true;

    ui.body.appendChild(makeField('组名', nameInput));
    ui.body.appendChild(makeField('文件夹', folderSelect));
    ui.body.appendChild(newFolderField);
    ui.body.appendChild(warn);

    const refresh = () => {
      const name = nameInput.value.trim();
      const conflict = Boolean(name) && name !== original && taken(name);
      warn.hidden = !conflict;
      warn.textContent = conflict ? `库里已经有「${name}」了，换个名字。` : '';
      ui.submitBtn.disabled = conflict;
    };

    const syncNewFolder = () => {
      const isNew = folderSelect.value === '__new__';
      newFolderField.hidden = !isNew;
      if (isNew) newFolderInput.focus();
    };

    nameInput.addEventListener('input', refresh);
    folderSelect.addEventListener('change', syncNewFolder);

    const submit = () => {
      const name = nameInput.value.trim();
      if (!name) {
        warn.hidden = false;
        warn.textContent = '名字不能空着。';
        nameInput.focus();
        return;
      }
      if (name !== original && taken(name)) {
        refresh();
        return;
      }
      let folder = folderSelect.value;
      if (folder === '__new__') {
        folder = newFolderInput.value.trim();
        if (!folder) {
          warn.hidden = false;
          warn.textContent = '新建文件夹的话，把文件夹名填上。';
          newFolderInput.focus();
          return;
        }
      }
      ui.close('submitted');
      resolve({ name, folder });
    };

    ui.submitBtn.addEventListener('click', submit);
    [nameInput, newFolderInput].forEach((input) => {
      input.addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
          event.preventDefault();
          submit();
        }
      });
    });

    refresh();
  });
}

/**
 * 只建一个文件夹。
 * @param {{folders?:string[]}} options
 * @returns {Promise<{folder:string}|null>}
 */
export function openNewFolderDialog(options = {}) {
  const folders = Array.isArray(options.folders) ? options.folders : [];

  return new Promise((resolve) => {
    const ui = mountDialog({
      title: '新建文件夹',
      describe: '文件夹只影响左栏怎么归类；宏组本身还是一个整体。',
      submitText: '创建',
      onCancel: () => resolve(null),
    });

    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'sd-input';
    nameInput.placeholder = '例如：开场、连招/BOSS';
    nameInput.maxLength = 60;
    nameInput.autocomplete = 'off';
    nameInput.spellcheck = false;

    const warn = document.createElement('p');
    warn.className = 'sd-warn';
    warn.hidden = true;

    ui.body.appendChild(makeField('文件夹名', nameInput, '同名文件夹会合并，不会重复建。'));
    ui.body.appendChild(warn);

    const submit = () => {
      const value = nameInput.value.trim();
      if (!value) {
        warn.hidden = false;
        warn.textContent = '写个文件夹名。';
        nameInput.focus();
        return;
      }
      if (folders.includes(value)) {
        warn.hidden = false;
        warn.textContent = `「${value}」已经存在了，宏组直接放进去就行。`;
        return;
      }
      ui.close('submitted');
      resolve({ folder: value });
    };

    ui.submitBtn.addEventListener('click', submit);
    nameInput.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        submit();
      }
    });
  });
}

/**
 * 插入一个宏组时先给它起名。
 * @param {{name?:string}} options
 * @returns {Promise<{name:string}|null>}
 */
export function openGroupNameDialog(options = {}) {
  const initial = String(options.name || '新组').trim() || '新组';

  return new Promise((resolve) => {
    const ui = mountDialog({
      title: '新建宏组',
      describe: '宏组只是编辑期的抽屉：存盘时会自动摊平成真实的动作，不影响宏文件格式。',
      submitText: '建组',
      onCancel: () => resolve(null),
    });

    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'sd-input';
    nameInput.value = initial;
    nameInput.placeholder = '例如：开场 / 连招A';
    nameInput.maxLength = 60;
    nameInput.autocomplete = 'off';
    nameInput.spellcheck = false;

    const warn = document.createElement('p');
    warn.className = 'sd-warn';
    warn.hidden = true;

    ui.body.appendChild(makeField('宏组名', nameInput, '插进序列以后，双击宏组还能随时改。'));
    ui.body.appendChild(warn);

    const submit = () => {
      const name = nameInput.value.trim();
      if (!name) {
        warn.hidden = false;
        warn.textContent = '给宏组起个名字。';
        nameInput.focus();
        return;
      }
      ui.close('submitted');
      resolve({ name });
    };

    ui.submitBtn.addEventListener('click', submit);
    nameInput.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        submit();
      }
    });
  });
}
