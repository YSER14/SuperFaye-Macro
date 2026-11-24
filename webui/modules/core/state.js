export const state = {
  macros: [],
  macroTree: { name: '宏', path: '宏', folders: [], files: [] },
  selectedMacro: null,
  editorRows: [],
  selectedActionPath: null,
  selectedLineKind: 'action',
  // 多选（点一下加入、再点一下移出）：拖拽时整批一起搬
  selectedActionPaths: [],
  // 当前视图里多选亮起的方块数（时间轴的 ↓↑ 合并块算一项）
  multiSelectVisibleCount: 0,
  draggedActionPath: null,
  draggedActionPaths: [],
  expandedLoopPathKeys: new Set(),
  inlineEditingPathKey: null,
  // 动作列表有改动但还没写进宏文件（触发键跑的是磁盘上的版本）
  editorDirty: false,
  consoleLastId: 0,
  consoleLineCount: 0,
  currentTab: 'macro',
  showDebugParams: false,
  // 多方案（绑定组）：一个方案＝一整套触发键绑定，数量不限
  schemes: [],
  activeSchemeId: null,
  activeSchemeName: '',
  // 「现有绑定」里当前点中的那一行（轮询重绘时要保持高亮）
  selectedBindingTrigger: null,
  activeRecordPathInput: 'simplifyInput',
  runtimeProfile: {
    speed_factor: 1,
    random_enabled: false,
    random_min_ms: 0,
    random_max_ms: 0,
    low_cpu_enabled: false,
    low_cpu_time_period: true,
  },
  recordServiceRunning: false,
  recordHotkeys: {
    arm: 'f7',
    stop: 'f8',
    save: 'f9',
    clear: 'f10',
  },
  batchCommentMode: false,
  // 动作列表显示方式：'block' 积木块（默认） | 'timeline' 罗技式方块时间轴
  actionViewMode: 'block',
  // 左栏「我的宏组」按文件夹折叠：存的是被收起的文件夹名（未分类用空串）
  groupFoldersCollapsed: new Set(),
  toastTimer: null,
  bootstrapDone: false,
  pollConsoleBusy: false,
  pollTimerId: null,
  snapshotTimerId: null,
  bindingTimerId: null,
  listenerTimerId: null,
  statusToastTs: {},
};
