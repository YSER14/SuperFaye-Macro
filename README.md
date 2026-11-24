# SuperFaye-Macro

[![License](https://img.shields.io/github/license/YSER14/SuperFaye-Macro?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4?style=flat-square&logo=windows&logoColor=white)](#环境要求)
[![Python](https://img.shields.io/badge/python-3.12%20x64-3776AB?style=flat-square&logo=python&logoColor=white)](#环境要求)
[![Tests](https://img.shields.io/badge/tests-101%20cases-brightgreen?style=flat-square)](#测试)

**Windows 键鼠宏编辑器**：把录制下来的操作、手写的宏串、或者从别处搬来的 XMBC 宏，变成看得懂、改得动的动作列表与节点流程图，再绑定到按键上直接跑。

界面是一个本地 WebView 窗口（Python + pywebview），不用开浏览器，也不用联网。

---

## 为什么不是又一个「一行宏」

大多数宏工具里，一条宏就是一串括号魔法或一大坨 JSON，想把「等 300 毫秒」改成「等 150」都得数括号。SuperFaye-Macro 把这层拆开：

- **看得见** —— 动作块、时间轴、节点画布三种视图，每一步按键、每一次延迟都摊开摆着。
- **改得动** —— 拖拽排序、成组管理、参数面板直接改数值，撤销重做跟着走。
- **搬得进** —— 现成的 XMBC `{TAG}` 宏串和旧版 JSON 宏可以直接导入，自动反编译成节点图，而不是让你重画一遍。
- **管得住** —— 多套触发方案互不干扰，每个绑定单独设重复次数和触发模式，随时一键停全部宏。

## 功能

### 宏编辑与录制

- 录制真实键鼠操作，生成可读的动作列表；轨迹和按键可以分开保留（`仅按键` / `仅轨迹`）。
- 动作块库 + 拖拽编排，支持多选、成组、复制粘贴、块级删除与撤销重做。
- 时间轴视图按真实时间轴查看节奏，`倍速执行` 试跑，不必反复绑键。
- 录制轨迹可化简（`化简输出`、`合并输出`），把几百个采样点压成能看懂的段落。
- 3D 视角录制：记录带视角变化的操作（例如游戏内镜头转动），回放时按同一节奏还原。

### 触发方案

- 为不同用途保存整套键鼠绑定（全局设置、绑定方案、宏组库各自独立成文件）。
- 每个绑定可设重复次数与触发模式；运行中的绑定在列表里高亮。
- 全局监听默认关闭，需要键鼠触发时才点 `启动全局监听`；`停止全部宏` 兜底防止卡键。
- 执行过程中保留物理键鼠干预能力，长宏跑到一半也能手动接管。

### 自动化流程（节点画布）

- 可视化流程图：连线即顺序，分支、循环、状态机、调用子宏都有独立节点。
- 节点按 9 个分类注册，含动作、控制、流程、变量、检测、监视、预设、脚本块与工具。
- 执行器带任务池，支持同步/异步调用子宏、变量作用域、类型转换与数学运算。
- 检测与监视类节点覆盖颜色检测、图像匹配、OCR 识别，配合控制类的条件与等待节点做轮询和条件等待。

### 宏导入与反编译

- 导入 XMBC `{TAG}` 宏串或原生 JSON 宏行，一键拆成节点图。
- 反编译遵循「不静默丢东西」原则：不可表达的部分变成画布注释 + 警告，而不是被悄悄删掉。

### AI 草稿助手

- 连接任意 OpenAI 兼容接口，生成动作或流程草稿，在编辑器里预览后再应用。
- 默认上下文为「无」，不主动外发任何编辑内容；密钥只存在当次运行的内存里。

### 输入精度

- 默认 SendInput 后端，普通权限即可用；需要穿透保护窗口时切换到 Interception 驱动。
- 高精度播放：进程/线程优先级提升、线程亲和性绑定、坐标换算与延迟补偿。

## 界面

启动后是一个窗口，顶部五个页签：

| 页签 | 用途 |
| --- | --- |
| **宏执行绑定** | 选择宏、设触发键、试跑、查看运行状态与全局开关 |
| **宏编辑保存** | 动作列表、动作块库、时间轴、录制与轨迹化简 |
| **全局设置** | 参数、输入后端、精度与界面选项 |
| **全局控制台** | 运行日志与错误输出 |
| **自动化编辑器** | 节点画布、图流程编辑、AI 草稿 |

## 环境要求

| 项目 | 要求 |
| --- | --- |
| 系统 | 64 位 Windows 10 或 11 |
| Python | 64 位 **CPython 3.12**（3.11 及以下、32 位均不支持） |
| 运行时 | Microsoft Edge WebView2 Runtime（Win11 自带，Win10 多数已随 Edge 安装） |
| 网络 | 仅首次安装依赖时需要 |

## 快速开始

在项目目录用**普通权限**打开 PowerShell：

```powershell
.\start.bat
```

首次运行会做三件事：定位 64 位 Python 3.12 → 在 `%LOCALAPPDATA%\SuperFaye-Macro\runtime-<路径哈希>` 建独立环境 → 按 `bin/requirements.txt` 安装并校验固定版本的依赖（带 SHA-256 哈希锁定，首次会比较慢）。环境装在用户目录，不污染项目目录，也不会向系统 Python 里塞包。

只想准备环境、不启动界面：

```powershell
.\start.bat --check
```

`3_启动webui.bat` 是同一入口的快捷方式，转发到 `start.bat`。

### 三分钟试跑第一条宏

1. 打开 **宏执行绑定 → 导入宏文本**，粘贴下面这条（只等待 1 秒）：

   ```text
   {WAITMS:1000}
   ```

2. 选中生成的宏，填一个触发键，点 **绑定当前宏**，再点 **立即试跑一次**。
3. 想用真实键鼠触发，点侧栏 **启动全局监听**；结束后点 **停止全部宏**。

想改动作：进 **宏编辑保存**，用 `导入现成宏` 把内容放进动作列表，编辑后 `保存当前宏`。

## 宏格式

### XMBC 宏串

按 X-Mouse Button Control 的 `{TAG}` 语法书写，大小写敏感，可混排普通字符与按键：

```text
{LMB}{WAITMS:300}{LMB}
```

```text
{CTRL}c{WAITMS:10}d
```

```text
{PRESS}a{WAITMS:200}{RELEASE}a
```

最后一条按住 `a` 200 毫秒再松开：`{PRESS}` / `{RELEASE}` 修饰下一条按键，而不是单独成一行。

导入时支持识别的标签：

| 分组 | 标签 |
| --- | --- |
| 鼠标按键 | `LMB` `RMB` `MMB` `MB4` `MB5` `XMB1` `XMB2` |
| 滚轮 | `MWUP` `MWDN` |
| 修饰键 | `CTRL` `ALT` `SHIFT` `WIN` |
| 时序 | `WAIT` `WAITMS` `HOLD` `HOLDMS` |
| 键码 | `VKC` `SC` `SCE`，以及普通字符键 |
| 按键状态 | `PRESS` `RELEASE` `CLEAR` |
| 窗口与光标 | `ACTIVATE` `ACTIVATETOP` `POSTWM` `SENDWM` `CURSORBUSY` `CURSORDEFAULT` `CB` |
| 图层与锁定 | `FLUSH` `LAYER` `LOCKX` `LOCKY` `LOCKXY` `UNLOCKX` `UNLOCKY` `UNLOCKXY` `LOCKC` `INVERTX` `INVERTY` `INVERTXY` `OD` `OR` `OU` `EXT` `TILTL` `TILTR` |

其中 24 个标签（窗口激活、光标状态、图层切换、坐标锁定与反转、倾斜滚轮等）在节点图里没有等价动作，导入后会被转成**画布注释 + 警告**保留原意，而不是假装成功。

### 原生 JSON 宏行

项目内部格式，一行一个动作，`import` 可嵌套引用其他宏：

```json
[
  ["kd", "a", 0],
  ["ku", "a", 30],
  ["wait", 200],
  ["md", "left", 0],
  ["mu", "left", 40],
  ["view", [120, -40], 500],
  ["scroll", -3, 20],
  ["loop", 3, [["wait", 100]]],
  ["import", "公共/常用前置宏.json"],
  ["note", "这一段是说明文字"]
]
```

支持的指令：`kd` `ku` `md` `mu` `wait` `view` `scroll` `loop` `import` `note`。

## 自动化节点

节点定义由 `automation/nodes/` 下的模块注册到 `automation/registry.py`，9 个分类共 43 个节点：

| 分类 | 节点 |
| --- | --- |
| **flow** 流程 | 开始、结束 |
| **action** 动作 | 按键动作、文本输入、鼠标移动、鼠标点击、鼠标按键（原地）、鼠标滚轮、相对位移、等待 |
| **control** 控制 | 条件分支、计数循环、条件循环、等待条件、跳转、序列检测、状态机 |
| **variable** 变量 | 设置变量、数学运算、字符串操作、比较、类型转换 |
| **utility** 工具 | 数字常量、字符串常量、布尔常量、数学函数、数学运算、逻辑运算、数值比较、字符串操作、条件选择、延迟、坐标书签 |
| **preset** 预设 | 点击坐标、找图点击、读取数字文本 |
| **script_block** 脚本块 | 调用宏（同步）、调用宏（异步） |
| **detection** 检测 | 图像匹配、颜色检测、OCR 识别、屏幕状态 |
| **monitor** 监视 | 颜色监视、OCR 监视、定时器 |

`数学运算` 与 `字符串操作` 在变量、工具两类下各注册一次，因此是 45 处注册、43 个不同节点。

节点图保存为 JSON（`{"meta", "variables", "settings", "graph"}`），可以纳入版本管理。

## AI 助手

从 **宏编辑保存** 或 **自动化编辑器** 打开 **AI 助手 → 连接**，填 OpenAI 兼容接口地址后 `保存连接`：

- 远程地址要求 HTTPS，本机回环地址允许 HTTP。
- 界面里填的 API 密钥只留在本次运行的内存中；也可以用环境变量 `MACRO_AI_BASE_URL` 和 `MACRO_AI_API_KEY`。
- 默认发送上下文为 **无**。只有你主动勾选编辑上下文时，模型服务才会收到对应的宏内容。
- 本机 WorkBuddy 场景可用 `MACRO_AI_WORKBUDDY_SETTINGS` 指向其设置文件，读取的密钥只用于该本机接口。

生成结果先在 `草稿` 页预览，确认后再应用到动作列表或画布。

## 管理员窗口与 Interception

自动化目标窗口如果以管理员身份运行，先**用普通权限**跑完 `.\start.bat --check`（装依赖不要提权），再按需以管理员身份启动 SuperFaye-Macro，否则两边权限不对等，按键发不出去。

Interception 驱动默认关闭，启用步骤：

1. 从 [Interception 官方发行页](https://github.com/oblitum/Interception/releases) 下载并解压，使安装器落在 `Interception/command line installer/install-interception.exe`。
2. 以管理员身份运行 [驱动安装脚本](bin/安装驱动_管理员运行_l.bat)，按提示重启。
3. 在 **全局设置** 中启用 Interception，并按需单独开启键盘或鼠标驱动。

驱动目录和二进制不进版本库（见 `.gitignore`），需要时自行下载。

## 本地数据与隐私

| 数据 | 默认位置 |
| --- | --- |
| 宏文件与保存的录制 | `宏/` |
| 自动化流程 | `自动化脚本/` |
| 全局设置、绑定方案、宏组库、AI 连接 | 项目根目录下各自的 `webui_*.json` |
| WebView 存储 | `appdata/webview/` |
| 运行日志 | `logs/` |

以上路径默认都在 `.gitignore` 里，不会被误提交。

录制期间请避开密码和其他敏感输入；分享宏或流程前，留意其中的文字、文件路径、窗口标题和录制内容。

## 项目结构

```text
start.bat                启动入口：定位 Python、建环境、装依赖、拉起界面
3_启动webui.bat          同上，快捷转发
webui_app.py             pywebview 窗口 + 本地服务，挂载各 bridge API
宏执行框架3.2.py          单文件宏执行核心：触发监听、播放、ESC 终止
automation/              节点注册表、图编译器、执行引擎、检测引擎、单元测试
  nodes/                 9 类节点的定义与处理函数
  handlers/              执行处理器
  detection/             图形、颜色、OCR 与序列检测
  tests/                 101 项单元测试
bridge/                  WebView 与后端之间的接口层（绑定、方案、录制、参数、AI、键鼠嗅探）
core/                    路径、参数、响应封装与 AI 模型客户端
engine/                  输入后端（SendInput / Interception）、坐标换算、精度控制
macro_io/                宏解析、反向编译、宏树读写
recorder/                录制、轨迹化简、3D 视角记录
webui/                   前端页面（index.html / automation_editor.html）与模块、图标字库
bin/                     依赖哈希清单与驱动安装脚本
```

## 测试

101 项单元测试，基于标准库 `unittest`，跑在合成数据上，不需要真实键鼠。

先准备环境，再跑整套：

```powershell
.\start.bat --check
$py = Get-ChildItem "$env:LOCALAPPDATA\SuperFaye-Macro\runtime-*\Scripts\python.exe" | Select-Object -First 1 -ExpandProperty FullName
& $py -m unittest discover -s automation/tests -t . -v
```

覆盖图编译器、执行引擎与处理器、宏触发、键盘播放、反向编译、AI 草稿与发布安全检查（不含本地路径、凭据与个人数据）。其中 AI 草稿相关的 13 项依赖 `pydantic-ai`，需要在准备好的环境里跑；其余 100 项不依赖第三方包也能通过。

## 常见问题

**提示找不到 Python 3.12**
装 64 位 CPython 3.12 后重开终端。脚本会依次尝试 `py -3.12`、`python`、`python3`、常见安装路径和 `uv` 的 Python 目录。

**装依赖时报「请先用普通权限运行 start.bat --check」**
说明当前在管理员窗口里且环境不完整。关掉管理员窗口，用普通权限装完依赖，再按需提权启动。

**提权后按键没反应**
确认目标窗口与 SuperFaye-Macro 权限一致；保护类窗口还需要启用 Interception 后端。

**窗口白屏或空白**
缺 WebView2 Runtime。装一次 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) 即可。

**想彻底重置环境**
删掉 `%LOCALAPPDATA%\SuperFaye-Macro\` 下对应的 `runtime-<哈希>` 目录，重新跑 `start.bat`。

## 参与开发

- Python 侧遵循 PEP 8 与 4 空格缩进；前端为原生 ES Module，不引入构建链。
- 新增节点：在 `automation/nodes/` 里写定义与处理函数，注册到 `registry.py`，补一条流程测试。
- 改动执行核心后请跑完整单测；涉及输入后端的改动建议在虚拟机或独立账号里验证。
- 提交前请确认 `宏/`、`logs/`、`webui_*.json` 等本机数据没有被带进提交。

## 来源与许可

本项目以 [MIT 许可证](LICENSE)发布，版权归 YS14。

项目基于 [Masclrin/MacroDriver-WebUI](https://github.com/Masclrin/MacroDriver-WebUI) 开发。上游仓库未声明适用许可证，本仓库的 MIT 授权仅覆盖本项目自身的修改与新增部分，不改变上游代码的版权归属；商用或再分发上游代码前请先取得上游作者授权，并以其说明为准（[上游 README](https://github.com/Masclrin/MacroDriver-WebUI/blob/716160d3cd25631dadd788a49045651620e73883/README.md)）。

自动化编辑器使用 Font Awesome Free 6.5.0 的本地样式和字库，组件附带其[许可证](webui/vendor/fontawesome/LICENSE.txt)。

## 使用提醒

宏与自动化工具可能改变其他软件的行为。请只在自己有权操作的环境中使用，并遵守目标程序与平台的服务条款；因使用本工具产生的后果由使用者自行承担。
