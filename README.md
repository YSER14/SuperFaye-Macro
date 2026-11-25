# SuperFaye-Macro

Windows 键鼠宏控制台与自动化流程编辑器（Rust + WebView2 重构版）：整合宏录制、分组编辑、按键触发方案、全局参数调优、节点图自动化编排，以及基于本地/云端 OpenAI 兼容接口的 AI 草稿助手。

## 主要功能

- **宏录制与轨迹化简**：支持 3D 视角位移与键盘鼠标动作同步录制，支持动作延迟统计与轨迹化简、按键/轨迹合成输出。
- **宏编辑器与分组库**：可视化编辑动作序列与组合块，支持宏循环与跨文件引用校验，支持一键导入/反向解析 XMBC 与 JSON 宏文本。
- **触发方案与并发执行**：支持按键绑定、连发与状态控制、多线程宏调度，以及基于系统坐标或游戏窗口的高精度输入模拟。
- **节点图自动化编排**（`automation_editor.html`）：支持可视化连线编排流程节点、屏幕区域取色与模板匹配、Windows OCR 文字识别、条件判断与窗口查找。
- **AI 助手**（可选，需用户自行连接接口）：支持在隔离会话内结合当前宏文本或自动化节点图生成局部草稿、预演改动并一键撤销（详见「安全说明」）。

## 运行环境

- **操作系统**：Windows 10 / Windows 11（x64）
- **Web 视图运行时**：Microsoft Edge WebView2 Runtime（Windows 11 默认自带；如缺失可从微软官网安装 Evergreen 运行时）
- **构建工具链**（源码编译时）：Rust（`stable-x86_64-pc-windows-gnu`，见 `rust-toolchain.toml`）与 MinGW-w64 UCRT64 / WinLibs 工具链

## 快速开始

### 1. 运行预编译程序或本地构建

```bat
:: 编译发布版并复制可执行文件到项目根目录
build.bat
```

或使用 Cargo：

```powershell
cargo test
cargo build --release
```

双击或运行 `start.bat` 启动桌面控制台。首次运行时会自动在项目根目录初始化：
- `宏/`：存放用户录制的 `.json` 宏文件
- `自动化脚本/`：存放 `.auto.json` 自动化流程文件
- `图片模板/`：存放图像识别节点使用的模板图片
- `appdata/` & `logs/`：本地 WebView2 状态、助手会话历史与运行日志

### 2. AI 助手配置（可选）

AI 助手默认**未预设任何服务器地址或密钥**。如需启用，可在界面右上角打开模型设置，手动填写兼容 OpenAI `/v1/chat/completions` 格式的接口地址：
- **远程接口**：仅允许 `https://` 地址；
- **本机模型服务**：允许 `http://` 访问回环地址（如 `127.0.0.1`、`localhost`、`::1`）；
- **环境变量覆盖**（可选）：可通过 `MACRO_AI_BASE_URL` 与 `MACRO_AI_API_KEY` 注入启动环境凭据。界面输入的密钥仅保存在当前进程内存中，不写入磁盘配置文件。

### 3. 可选硬件级驱动：Interception

默认参数已将 Interception 驱动开关设为关闭（`USE_INTERCEPTION = false`），直接使用系统 `SendInput` API 即可工作。  
若需使用底层的 Interception 驱动，请自行从官方发布渠道获取合规签名驱动并将 `interception.dll` 放置于 `Interception/library/x64/interception.dll` 后再开启开关。

## 安全与离线说明

- **零内置个人数据与默认预设**：仓库不包含任何用户私人宏脚本、自动化脚本、账号凭据、本机密钥文件或绝对本机路径。
- **全离线静态前端资源**：WebUI（主控台与节点图编辑器）所有样式与 Font Awesome 图标字体均本地打包于 `webui/vendor/fontawesome/`，无外链 CDN 请求，并启用严格 CSP 策略。
- **目录与路径安全隔离**：所有宏读写、模板读取、自动化脚本加载与 WebUI 本地资源访问均经过路径规范化校验，拒绝父级目录穿越（`..`）、绝对路径越界、备用数据流（ADS `:`）、Windows 保留设备名及符号链接/重解析点。
- **独立安全回归测试**：可通过 `cargo test` 验证路径隔离、默认值空白检查、AI 接口校验及 WebUI 资源合规性。

## 目录结构

```text
├── assets/param_defaults.json   # 全局默认参数与 UI 字段 Schema
├── build.rs / build.bat         # Windows 资源（图标）嵌入与发布构建脚本
├── Cargo.toml / Cargo.lock      # Rust 依赖与工作区配置
├── src/
│   ├── assistant/               # AI 模型客户端、只读/草稿工具与本地会话历史
│   ├── automation/              # 自动化引擎、节点处理器、OCR 与模板匹配
│   ├── bridge/                  # WebView2 JS ↔ Rust IPC 接口层与参数存储
│   ├── core/                    # 路径安全解析、环形控制台日志与响应封装
│   ├── engine/                  # 热键钩子、坐标系转换、SendInput / Interception 后端
│   ├── executor/                # 宏编译器、多触发调度运行器
│   ├── macro_io/                # 带注释宏解析器与 XMBC 反向编译
│   ├── recorder/                # 3D 录制服务与轨迹化简工具
│   └── webview_host.rs          # WebView2 桌面容器与受限自定义协议资源服务
└── webui/                       # 控制台、节点图编辑器 HTML/CSS/JS 与本地 FontAwesome 资源
```

## 开源许可与致谢

- 本项目基于 [MIT License](LICENSE) 开源（Copyright (c) 2026 YS14）。
- 最初思想与部分桌面框架交互灵感致谢 [Masclrin/MacroDriver-WebUI](https://github.com/Masclrin/MacroDriver-WebUI)。
- WebUI 图标字体来自 Font Awesome Free 6.5.0，授权详情见 [webui/vendor/fontawesome/LICENSE.txt](webui/vendor/fontawesome/LICENSE.txt)。
