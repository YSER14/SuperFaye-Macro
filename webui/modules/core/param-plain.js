// 全局参数 · 人话文案层
// ---------------------------------------------------------------------------
// 后端（bridge/metadata.py）里每一项都带「标签 / 变量名 / 说明 / 建议 / 默认值」，
// 但标签本身是工程术语（LAG_KP、FF_SCALE、Interception…）。这一层不删除任何
// 后端信息，只负责回答两个问题：
//   1. 这一项该归到哪一组（用户脑子里的分类，而不是代码里的 section）；
//   2. 用普通人能读的一句话，说清「它是什么、动了会怎样」。
// 原始标签、变量名、说明、建议、示例、默认值仍然全量展示（折叠在「技术细节」里），
// 所以这里没有命中也不会丢东西 —— plainFor() 会退回后端字段。
// ---------------------------------------------------------------------------

/** 分组顺序即界面顺序。note 是分组标题下的一行说明。 */
export const PARAM_GROUPS = [
  {
    id: 'keys',
    title: '先设这个：怎么让宏立刻停下',
    note: '紧急时刻靠这几个键。设成手指够得到、又不容易误触的位置。',
  },
  {
    id: 'rhythm',
    title: '跑多快、像不像真人',
    note: '整体速度和随机抖动。越接近人手的自然波动，规律越不明显。',
  },
  {
    id: 'precision',
    title: '准不准：时间误差修正',
    note: '宏跑久了会越跑越偏。这里决定要不要自动把偏差补回来，以及补多狠。',
  },
  {
    id: 'output',
    title: '动作是怎么送到游戏里的',
    note: '发送通道和打包方式。动作被吞、连点失效时优先看这里。',
  },
  {
    id: 'resource',
    title: '电脑负担与稳定性',
    note: '长时间挂机、多开时用来平衡 CPU 占用和精度。',
  },
  {
    id: 'record',
    title: '录制：按什么录、存到哪',
    note: '快捷键、采样密度和保存位置。',
  },
  {
    id: 'simplify',
    title: '鼠标轨迹怎么简化',
    note: '录下来的轨迹点太多，简化之后宏更短、跑起来更稳。',
  },
  {
    id: 'offline',
    title: '离线化简预设',
    note: '宏编辑器的「简化工具」用的那套参数，日常执行不受影响。',
  },
  {
    id: 'editor',
    title: '自动化编辑器快捷键',
    note: '在编辑器里运行、暂停、停止脚本时按的键。',
  },
  {
    id: 'device',
    title: '注入设备微调',
    note: '只有出现连点、漏按、点击错位时才需要动，平时别碰。',
  },
  {
    id: 'behavior',
    title: '其它行为',
    note: '',
  },
  {
    id: 'debug',
    title: '未开放的实验项',
    note: '还没做完的功能，默认不显示。',
  },
];

/**
 * 逐字段人话文案。
 *   group       —— 归属分组 id
 *   title       —— 界面主标题（人话）
 *   plain       —— 一句话解释：是什么 / 动了会怎样
 *   warn        —— 可选：动了容易出问题的提醒
 *   optionHints —— 可选：下拉框每个选项的一句话说明
 */
export const PARAM_PLAIN = {
  // ── 按键 ────────────────────────────────────────────────────────────────
  'executor.GLOBAL_PAUSE_TOGGLE_KEY': {
    group: 'keys',
    title: '暂停 / 继续',
    plain: '按一下所有正在跑的宏都停住，再按一下接着跑，进度不会丢。',
  },
  'executor.STOP_MACRO_KEY': {
    group: 'keys',
    title: '全部停止',
    plain: '直接把所有宏掐掉，不会恢复。出问题时按它。',
  },
  'executor.MACRO_RELOAD_KEY': {
    group: 'keys',
    title: '重新载入宏文件',
    plain: '改了宏文件以后按一下就能生效。留空则每次触发宏时才重新读文件。',
  },

  // ── 速度 / 拟人化 ───────────────────────────────────────────────────────
  'executor.EXEC_SPEED_FACTOR': {
    group: 'rhythm',
    title: '整体倍速',
    plain: '把整段宏按比例快放或慢放，1 是原速，2 就是两倍速。',
  },
  'executor.ENABLE_RUNTIME_RANDOM_OFFSET': {
    group: 'rhythm',
    title: '每个动作加一点随机延迟',
    plain: '每次多等零点几毫秒，让节奏不像机器那么整齐。',
  },
  'executor.RUNTIME_RANDOM_OFFSET_MIN_MS': {
    group: 'rhythm',
    title: '随机延迟 · 最少加多少毫秒',
    plain: '随机范围的下限，0 表示可能一点都不加。',
  },
  'executor.RUNTIME_RANDOM_OFFSET_MAX_MS': {
    group: 'rhythm',
    title: '随机延迟 · 最多加多少毫秒',
    plain: '随机范围的上限。建议压在 6ms 以内，太大宏会变慢。',
  },
  'executor.ENABLE_RANDOM_DELAY_ADJUST': {
    group: 'rhythm',
    title: '按动作长短分档随机',
    plain: '短动作少加、长动作多加，比统一加一个固定值更像人手。',
  },
  'executor.RANDOM_DELAY_ADJUST_RULES_BY_DELAY_MS': {
    group: 'rhythm',
    title: '分档表：哪一档加多少',
    plain: '按原始延迟落在哪一段，决定这一档额外加几毫秒。',
  },
  'executor.ABSOLUTE_TIMELINE_USE_DELAY_ADJUST': {
    group: 'rhythm',
    title: '在精确时间线里也使用分档随机',
    plain: '想要更自然就打开；想追求绝对稳定的复现就关掉。',
  },

  // ── 时间精度 ────────────────────────────────────────────────────────────
  'executor.TIMELINE_EXECUTION_MODE': {
    group: 'precision',
    title: '时间怎么排',
    plain: '决定所有动作是卡在同一条时间轴上，还是一个接一个往后顺延。',
    optionHints: {
      absolute: '所有动作卡同一时间轴，延迟最低；但间隔极短的动作可能后发先至。',
      relative_compensated: '前一个动作结束后再排下一个，稳、不会乱序，代价是略慢一点。',
    },
  },
  'executor.ABSOLUTE_TIMELINE_ALLOW_EARLY_MS': {
    group: 'precision',
    title: '允许提前多少毫秒动手',
    plain: '专门用来抵掉 Python 循环本身的开销，让动作卡得更准。',
  },
  'executor.ABSOLUTE_TIMELINE_WAIT_CHUNK_MS': {
    group: 'precision',
    title: '等待时多久检查一次时间',
    plain: '间隔太粗会抖，太细更吃 CPU。',
  },
  'executor.LAG_COMPENSATION_ENABLED': {
    group: 'precision',
    title: '自动修正累积误差',
    plain: '长宏跑久了会整体往后拖，打开后每一步都把偏差补一点回来。',
  },
  'executor.LAG_USE_PI_CONTROLLER': {
    group: 'precision',
    title: '偏差按大小自动加减',
    plain: '偏得多就补得狠，偏得少就轻补，而不是永远补同一个量。',
  },
  'executor.LAG_KP': {
    group: 'precision',
    title: '修正力度',
    plain: '发现偏差后一次补多猛。调大追得快，但容易补过头来回晃。',
    warn: '一次调 0.05 上下试，别一上来翻倍。',
  },
  'executor.LAG_KI': {
    group: 'precision',
    title: '长期偏差的修补力度',
    plain: '专门对付那种一直补不回来的慢性偏差；调太大偏差会慢慢累积过头。',
  },
  'executor.LAG_ERROR_TRIGGER_MS': {
    group: 'precision',
    title: '偏差超过多少才动手',
    plain: '小于这个值当成正常抖动，不去修，避免一惊一乍。',
  },
  'executor.LAG_ERROR_TARGET_MS': {
    group: 'precision',
    title: '想把偏差压到多少以内',
    plain: '修正的目标值，通常要比上面的触发阈值更小。',
  },
  'executor.LAG_MAX_STEP_COMP_PCT': {
    group: 'precision',
    title: '单步最多补多少（比例）',
    plain: '给单次修正封顶，防止一步补太猛把整段节奏带崩。',
  },
  'executor.LAG_MIN_STEP_DELAY_MS': {
    group: 'precision',
    title: '每步至少留多长间隔',
    plain: '修正之后也不会把两个动作压到完全贴在一起。',
  },
  'executor.LAG_USE_FEEDFORWARD': {
    group: 'precision',
    title: '按节奏提前预判',
    plain: '不等误差出现就先预估下一拍，减少那种「慢半拍」的滞后感。',
  },
  'executor.FF_SCALE': {
    group: 'precision',
    title: '预判修正的整体强度',
    plain: '1 是按完整预判量补；调小更保守，调大更激进。',
  },
  'executor.FF_KP': {
    group: 'precision',
    title: '预判修正的力度',
    plain: '预判环节的灵敏度，越大越激进。',
  },
  'executor.FF_WINDOW_SIZE': {
    group: 'precision',
    title: '预判看最近几个动作',
    plain: '取多少步的历史来估节奏。取太少噪声大，取太多反应钝。',
  },
  'executor.FF_MAX_COMP_PCT': {
    group: 'precision',
    title: '预判一次最多补多少',
    plain: '给预判修正封顶，避免冲过头。',
  },
  'executor.MAX_MACRO_EXEC_TIME': {
    group: 'precision',
    title: '一个宏最多跑多久（秒）',
    plain: '超时自动停，防止死循环把电脑挂死。',
  },
  'executor.MAX_COMPILED_ACTIONS': {
    group: 'precision',
    title: '一个宏最多多少条动作',
    plain: '条目上限，超过就拒绝编译执行。',
  },

  // ── 输出通道 ────────────────────────────────────────────────────────────
  'executor.USE_INTERCEPTION': {
    group: 'output',
    title: '用驱动级方式发送按键',
    plain: '比普通方式更难被游戏忽略。动作发不出去、时好时坏时可以来回切这一项定位。',
  },
  'executor.USE_INTERCEPTION_KEYBOARD': {
    group: 'output',
    title: '键盘走驱动方式',
    plain: '只影响键盘发送通道。',
  },
  'executor.USE_INTERCEPTION_MOUSE': {
    group: 'output',
    title: '鼠标走驱动方式',
    plain: '只影响鼠标发送通道。',
  },
  'executor.ENABLE_BATCH_SEND': {
    group: 'output',
    title: '连续动作打包一次发送',
    plain: '把紧挨着的几个动作合成一次提交，连发类宏明显更快更顺。',
  },
  'executor.BATCH_DELAY_THRESHOLD_MS': {
    group: 'output',
    title: '间隔多近的动作可以打包',
    plain: '0 表示只打包完全没有延迟的动作，最保守、不会打乱顺序。',
  },
  'executor.BATCH_MAX_SIZE': {
    group: 'output',
    title: '一包最多装几条动作',
    plain: '包太大单次提交会变慢，一般 16 上下就够。',
  },
  'executor.BATCH_INCLUDE_MOUSE_CLICK': {
    group: 'output',
    title: '鼠标点击也参与打包',
    plain: '关掉就只打包键盘。点击出现丢失或错位时先关这个试试。',
  },

  // ── 资源占用 ────────────────────────────────────────────────────────────
  'executor.ENABLE_LOW_CPU_EXECUTION': {
    group: 'resource',
    title: '省电模式（低占用执行）',
    plain: '等待时改用普通睡眠，挂机时 CPU 明显降下来，代价是精度略降。',
  },
  'executor.LOW_CPU_ENABLE_TIME_PERIOD': {
    group: 'resource',
    title: '省电模式下提高系统计时精度',
    plain: '让系统的睡眠更准一点，功耗会略微上升。',
  },
  'executor.LOW_CPU_FORCE_RELATIVE_MODE': {
    group: 'resource',
    title: '省电模式下自动改用相对时间线',
    plain: '绝对时间线很吃 CPU，省电时自动换成相对计时，长宏更耐磨。',
  },
  'executor.ENABLE_THREAD_CORE_BINDING': {
    group: 'resource',
    title: '把宏线程固定到指定 CPU 核心',
    plain: '线程不再被系统到处挪，多个宏同时跑时更稳定。',
  },
  'executor.CORE_BINDING_MAX_CORES': {
    group: 'resource',
    title: '最多占用几个核心',
    plain: '从 CPU 最后几个核心往前分配，不跟前台程序抢。',
  },
  'executor.CORE_BINDING_MAX_THREADS_PER_CORE': {
    group: 'resource',
    title: '每个核心最多放几个宏线程',
    plain: '满了就自动挪到最空闲的核心。',
  },
  'executor.ENABLE_REALTIME_PRIORITY': {
    group: 'resource',
    title: '让宏线程优先抢 CPU',
    plain: '精度更好，但可能拖慢别的程序，只在追求极限精度时开。',
  },
  'executor.ENABLE_PRO_AUDIO': {
    group: 'resource',
    title: '借音频引擎的高精度时钟',
    plain: '用音频那套低延迟定时器来排动作，更准但更吃资源。',
  },

  // ── 录制 ────────────────────────────────────────────────────────────────
  'recorder.ENABLE_RECORD_ACTIONS': {
    group: 'record',
    title: '记录键盘和鼠标按键',
    plain: '关掉之后就只记鼠标移动路线，不记按下松开。',
  },
  'recorder.ENABLE_RECORD_TRACK': {
    group: 'record',
    title: '记录鼠标移动路线',
    plain: '鼠标划过的轨迹也存下来，视角转动类操作必须有它。',
  },
  'recorder.ENABLE_RECORD_HOTKEY_ACTIONS': {
    group: 'record',
    title: '把快捷键动作也录进去',
    plain: '录制时按下的组合键当成宏动作记录，而不是触发别的功能。',
  },
  'recorder.START_MODE': {
    group: 'record',
    title: '怎么算「开始录」',
    plain: '按下开始键之后，是等几秒自己开始，还是等你按第一个键才开始。',
    optionHints: {
      delayed: '按下开始键后等「延迟秒数」再正式开始，留时间把鼠标移到位。',
      first_event: '按下开始键后待命，等你真的按下第一个键或移动鼠标才开始计时。',
    },
  },
  'recorder.START_DELAY_SECONDS': {
    group: 'record',
    title: '按开始后等几秒再录',
    plain: '给自己留一点准备时间。',
  },
  'recorder.RECORD_ARM_HOTKEY': {
    group: 'record',
    title: '开始录制的键',
    plain: '按它就进入录制待命。留空表示不设快捷键。',
  },
  'recorder.RECORD_STOP_HOTKEY': {
    group: 'record',
    title: '停止录制的键',
    plain: '按它结束本次录制。建议放在开始键旁边，方便急停。',
  },
  'recorder.RECORD_SAVE_HOTKEY': {
    group: 'record',
    title: '保存这次录制的键',
    plain: '按它把刚录的内容写成宏文件。',
  },
  'recorder.RECORD_CLEAR_HOTKEY': {
    group: 'record',
    title: '清空这次录制的键',
    plain: '录废了直接扔掉，不用存文件。',
  },
  'recorder.MAX_RECORD_SECONDS': {
    group: 'record',
    title: '录制最多回看多少秒',
    plain: '只保留最近这段时间的内容，更早的会被丢掉，防止内存越占越多。',
  },
  'recorder.MOUSE_SAMPLE_INTERVAL_MS': {
    group: 'record',
    title: '鼠标多久取一个点（毫秒）',
    plain: '越小轨迹越细腻，文件也越大。',
  },
  'recorder.MOUSE_MOVE_SCALE': {
    group: 'record',
    title: '鼠标移动距离倍数',
    plain: '录进来的位移乘上这个系数，用来匹配游戏里的灵敏度。',
  },
  'recorder.ENABLE_KEY_WHITELIST': {
    group: 'record',
    title: '只录白名单里的按键',
    plain: '打开后，没列出来的键不会被录进宏。',
  },
  'recorder.ALLOWED_KEYS': {
    group: 'record',
    title: '白名单里有哪些键',
    plain: '上面那个开关打开后才生效。',
  },
  'recorder.OUTPUT_DIR': {
    group: 'record',
    title: '录好的文件存到哪个文件夹',
    plain: '保存录制时按这个路径写文件。',
  },
  'recorder.OUTPUT_BASE_NAME': {
    group: 'record',
    title: '录好的文件名前缀',
    plain: '生成的宏文件名以它开头，方便一次录多份时区分。',
  },
  'recorder.BOOST_PROCESS_PRIORITY': {
    group: 'record',
    title: '录制时临时提高程序优先级',
    plain: '录制容易漏点时可以打开；平时不建议常开。',
  },
  'recorder.BIND_CPU_CORE': {
    group: 'record',
    title: '录制时固定在某个 CPU 核心',
    plain: '填 -1 表示不固定。',
  },

  // ── 轨迹简化 ────────────────────────────────────────────────────────────
  'recorder.ENABLE_TRACK_SIMPLIFY': {
    group: 'simplify',
    title: '录完自动简化轨迹',
    plain: '边录边压缩轨迹点，宏文件更小、回放更顺。',
  },
  'recorder.TRACK_SIMPLIFY_PIPELINE': {
    group: 'simplify',
    title: '自动简化用哪些步骤',
    plain: '按顺序执行的简化步骤列表。建议先保持默认。',
  },
  'recorder.SIMPLIFY_ANGLE_THRESHOLD_DEG': {
    group: 'simplify',
    title: '拐弯多大才算一个转折点',
    plain: '角度阈值。调大只保留明显的转向，调小会把轻微弯曲也当转折点。',
  },
  'recorder.SIMPLIFY_SPEED_RATIO_THRESHOLD': {
    group: 'simplify',
    title: '速度变化多少算一次变速',
    plain: '用来切分「匀速滑」和「突然甩」。',
  },
  'recorder.SIMPLIFY_SLIDING_WINDOW_SIZE': {
    group: 'simplify',
    title: '判断时参考前后几个点',
    plain: '窗口越大越平滑，越小越贴近原始手感。',
  },
  'recorder.SIMPLIFY_RDP_EPSILON': {
    group: 'simplify',
    title: '允许多大的形状误差',
    plain: '数值越大点删得越多、文件越小，但轨迹会更「方」。',
  },
  'recorder.SIMPLIFY_MERGE_GAP_MS': {
    group: 'simplify',
    title: '间隔多近的两段轨迹并成一段',
    plain: '把夹在中间的小停顿抹掉。',
  },
  'recorder.SIMPLIFY_MIN_MANHATTAN': {
    group: 'simplify',
    title: '多短的碎移动直接丢掉',
    plain: '过滤手抖级别的小位移。',
  },
  'recorder.SIMPLIFY_ALLOW_CROSS_ACTION_MERGE': {
    group: 'simplify',
    title: '允许跨按键合并轨迹',
    plain: '把两次按键之间的轨迹接成一条，能删掉更多冗余点。',
  },
  'recorder.ENABLE_ACTION_DELAY_EMBEDDING': {
    group: 'simplify',
    title: '把轨迹末尾的停顿并进按键延迟',
    plain: '少一条空白等待动作，时间轴更紧凑。',
  },
  'recorder.ACTION_EMBED_DELAY_MS': {
    group: 'simplify',
    title: '最多并入多少毫秒',
    plain: '上限，避免把关键等待吃掉。',
  },
  'recorder.ACTION_EMBED_MAX_RATIO_TO_NEXT_PRESS': {
    group: 'simplify',
    title: '最多占下一次按键间隔的几成',
    plain: '按比例兜底，防止并得太多导致节奏变形。',
  },
  'recorder.MIN_SPEED_PX_PER_SEC': {
    group: 'simplify',
    title: '慢到什么程度当成手抖',
    plain: '低于这个速度的移动当作噪声丢掉。',
  },

  // ── 离线预设 ────────────────────────────────────────────────────────────
  'simplify_tool.SCENE_PRESET': {
    group: 'offline',
    title: '用哪套预设',
    plain: '给宏编辑器里的「简化工具」选一套现成参数，只有选自定义时才读下面那些。',
    optionHints: {
      fps: '射击游戏视角：保留小幅度精细移动。',
      desktop: '桌面日常：压缩更狠，文件更小。',
      rpg3d: '3D 游戏：介于两者之间。',
      custom: '用下面自定义那一组参数。',
    },
  },
  'simplify_tool.CUSTOM_CFG.pipeline': {
    group: 'offline',
    title: '自定义：简化步骤',
    plain: '按顺序执行的步骤列表。',
  },
  'simplify_tool.CUSTOM_CFG.rdp_epsilon': {
    group: 'offline',
    title: '自定义：形状误差容差',
    plain: '越大删得越多、形状越方。',
  },
  'simplify_tool.CUSTOM_CFG.intent_angle_threshold_deg': {
    group: 'offline',
    title: '自定义：拐弯阈值（度）',
    plain: '多大角度才算一次真正的转向。',
  },
  'simplify_tool.CUSTOM_CFG.intent_speed_ratio_threshold': {
    group: 'offline',
    title: '自定义：变速阈值',
    plain: '速度变化多少算一次变速。',
  },
  'simplify_tool.CUSTOM_CFG.intent_min_dt_ms': {
    group: 'offline',
    title: '自定义：最短一段轨迹（毫秒）',
    plain: '短于这个长度的片段不单独成段。',
  },
  'simplify_tool.CUSTOM_CFG.sliding_window_size': {
    group: 'offline',
    title: '自定义：判断窗口大小',
    plain: '看前后几个点来判断趋势。',
  },
  'simplify_tool.CUSTOM_CFG.merge_gap_ms': {
    group: 'offline',
    title: '自定义：合并间隔（毫秒）',
    plain: '间隔多近的两段并成一段。',
  },
  'simplify_tool.CUSTOM_CFG.min_manhattan_after_simplify': {
    group: 'offline',
    title: '自定义：最短碎移动',
    plain: '多短的位移直接丢掉。',
  },
  'simplify_tool.CUSTOM_CFG.allow_cross_action_merge': {
    group: 'offline',
    title: '自定义：允许跨按键合并',
    plain: '把两次按键之间的轨迹接起来。',
  },
  'simplify_tool.CUSTOM_CFG.enable_action_delay_embedding': {
    group: 'offline',
    title: '自定义：末尾停顿并入延迟',
    plain: '把轨迹尾部等待并进按键延迟。',
  },
  'simplify_tool.CUSTOM_CFG.action_embed_delay_ms': {
    group: 'offline',
    title: '自定义：最多并入多少毫秒',
    plain: '并入量的上限。',
  },
  'simplify_tool.CUSTOM_CFG.action_embed_max_ratio_to_next_press': {
    group: 'offline',
    title: '自定义：占下一按键间隔比例',
    plain: '按比例兜底。',
  },

  // ── 编辑器快捷键 ────────────────────────────────────────────────────────
  'automation_editor.HOTKEY_ENABLED': {
    group: 'editor',
    title: '启用编辑器快捷键',
    plain: '关掉之后下面四个键都不生效，避免跟别的软件抢键。',
  },
  'automation_editor.HOTKEY_RUN': {
    group: 'editor',
    title: '运行脚本的键',
    plain: '在自动化编辑器里按它开始执行当前脚本。',
  },
  'automation_editor.HOTKEY_PAUSE': {
    group: 'editor',
    title: '暂停 / 继续的键',
    plain: '脚本跑到一半按它挂起，再按一下接着跑。',
  },
  'automation_editor.HOTKEY_STOP': {
    group: 'editor',
    title: '停止脚本的键',
    plain: '直接结束当前脚本。建议设成最好按的那个键。',
  },
  'automation_editor.HOTKEY_FULLSCREEN': {
    group: 'editor',
    title: '全屏预览的键',
    plain: '在编辑器里切换全屏看界面。',
  },

  // ── 注入设备微调 ────────────────────────────────────────────────────────
  'interception.INTERCEPTION_KEYBOARD_REDUNDANT_DEVICES': {
    group: 'device',
    title: '键盘虚拟设备数量',
    plain: '多准备一个备用虚拟键盘，某些游戏认不到主设备时补位。',
  },
  'interception.INTERCEPTION_KEYBOARD_SCAN_INTERVAL': {
    group: 'device',
    title: '多久重新扫一次键盘设备（秒）',
    plain: '热插拔键盘后多久能重新识别。',
  },
  'interception.INTERCEPTION_MOUSE_LEFT_DOWN_LIMITED': {
    group: 'device',
    title: '左键按下只发给一个设备',
    plain: '防止一次点击被游戏识别成两次。',
  },
  'interception.INTERCEPTION_MOUSE_LEFT_REDUNDANT_DOWN_DEVICES': {
    group: 'device',
    title: '左键按下用几个虚拟设备',
    plain: '配合上面那个限制模式使用。',
  },
  'interception.INTERCEPTION_MOUSE_LEFT_UP_LIMITED_RELEASE': {
    group: 'device',
    title: '左键抬起只发给一个设备',
    plain: '和按下同理，防止抬起丢事件导致「按着不放」。',
  },
  'interception.INTERCEPTION_MOUSE_LEFT_REDUNDANT_UP_DEVICES': {
    group: 'device',
    title: '左键抬起用几个虚拟设备',
    plain: '配合上面那个限制模式使用。',
  },

  // ── 其它行为 ────────────────────────────────────────────────────────────
  'webui.ALLOW_MULTI_MACRO_BIND_PER_KEY': {
    group: 'behavior',
    title: '一个键可以同时触发多个宏',
    plain: '默认关闭。确实需要同键并发时才打开，否则容易一次跑起来一堆。',
  },

  // ── 未开放 ──────────────────────────────────────────────────────────────
  'executor.ENABLE_TEST_TRACE': { group: 'debug', title: '步进测试追踪', plain: '开发用，记录每一步的时间偏差。' },
  'executor.TEST_TRACE_OUTPUT_MODE': { group: 'debug', title: '测试输出方式', plain: '最终汇总还是实时打印。' },
  'executor.ALLOW_LOCK_STYLE_KEY_LONG_PRESS': { group: 'debug', title: '锁定式长按', plain: '把长按当开关用。' },
  'executor.MACRO_BINDINGS': { group: 'debug', title: '触发键绑定数据', plain: '界面自动维护，不建议手改。' },
  'executor.MACRO_RUNTIME_SETTINGS': { group: 'debug', title: '触发键运行状态', plain: '界面自动维护，不建议手改。' },
  'recorder.ENABLE_ULTRA_LOW_JITTER_RECORD': { group: 'debug', title: '超低抖动录制', plain: '实验特性，采样更密。' },
  'recorder.ULTRA_LOW_JITTER_MOUSE_SAMPLE_INTERVAL_MS': { group: 'debug', title: '超低抖动采样间隔（毫秒）', plain: '只在超低抖动录制打开时有效。' },
  'recorder.ULTRA_LOW_JITTER_DISABLE_LOW_SPEED_FILTER': { group: 'debug', title: '关闭低速过滤', plain: '连手抖也一起录下来。' },
  'interception.INTERCEPTION_CAPTURE_PHYSICAL_INPUT': { group: 'debug', title: '捕获物理输入', plain: '把真实键鼠事件也拦下来。' },
};

/** 后端字段 → 人话信息（缺项时逐级回退，绝不丢字段）。 */
export function plainFor(field) {
  const entry = PARAM_PLAIN[field.field_id] || {};
  return {
    group: entry.group || 'behavior',
    title: entry.title || field.label || field.key,
    plain: entry.plain || field.desc || '',
    warn: entry.warn || '',
    optionHints: entry.optionHints || null,
  };
}
