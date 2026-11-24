# SuperFaye-Macro

SuperFaye-Macro 是一款 Windows 键鼠宏编辑器。你可以录制操作、拖拽编排动作，把现有 XMBC 宏串或 JSON 宏导入后继续编辑，并为不同用途保存独立的触发键方案。

## 你可以用它做什么

- **编辑和录制宏**。通过动作块或时间轴查看键鼠操作，整理动作组，也可以把刚录制的动作直接追加到编辑列表。
- **管理触发方案**。为不同任务保存整套键鼠绑定，并为每个绑定设置重复次数和触发模式。
- **编排自动化流程**。在节点画布里组合键鼠动作、条件分支、循环、屏幕取色和 OCR。
- **使用 AI 草稿**。连接自己选择的模型服务，生成动作或流程草稿，在编辑器里预览和应用。

## 快速开始

需要 **64 位 Windows 10 或 11**、**64 位 Python 3.12** 和 **Microsoft Edge WebView2 Runtime**。在项目目录用普通权限打开 PowerShell 并运行：

```powershell
.\start.bat
```

首次运行会从 PyPI 安装并校验固定版本的依赖。运行 `.\start.bat --check` 可以只准备环境并检查依赖导入。普通窗口可直接用默认的 SendInput 后端。

### 试跑一条宏

1. 进入 **宏执行绑定 → 导入宏文本**，粘贴下面的 XMBC 宏串。这个示例只等待 1 秒。

   ```text
   {WAITMS:1000}
   ```

2. 选中生成的宏，填入触发键，点击 **绑定当前宏**，再点 **立即试跑一次**。运行中的绑定会高亮显示。
3. 需要使用键鼠触发时，点击侧栏的 **启动全局监听**。可用 **停止全部宏** 结束正在运行的宏。

想改动作时，在 **宏编辑保存** 中选中已有宏，或用 **导入现成宏** 将内容加入动作列表，编辑后点击 **保存当前宏**。

## 管理员窗口与 Interception

自动化目标窗口如果以管理员身份运行，请先用普通权限完成 `.\start.bat --check`，再按需以管理员身份启动 SuperFaye-Macro。

Interception 默认关闭。要启用它，从官方发行页下载并解压，使安装器位于 `Interception/command line installer/install-interception.exe`。以管理员身份运行 [驱动安装脚本](bin/安装驱动_管理员运行_l.bat)，按提示重启。然后在 **全局设置** 中启用 Interception，并按需开启键盘或鼠标驱动。[Interception 官方发行页](https://github.com/oblitum/Interception/releases)

## AI 助手

从 **宏编辑保存** 或 **自动化编辑器** 打开 **AI 助手**，进入 **连接**，填写 OpenAI 兼容接口并点击 **保存连接**。远程地址使用 HTTPS，本机回环地址可以使用 HTTP。

- 界面输入的 API 密钥仅保存在当次运行的内存中。也可以用 `MACRO_AI_BASE_URL` 和 `MACRO_AI_API_KEY` 为指定接口提供密钥。
- 发送请求时，服务商会收到你的输入、近期对话和你选中的编辑上下文。默认上下文为 **无**。
- 在 **草稿** 页预览结果，确认后应用到动作列表或画布，再用编辑器保存与运行。

使用本机 WorkBuddy 时，可以通过 `MACRO_AI_WORKBUDDY_SETTINGS` 指定本机设置文件。读取的密钥只用于对应的本机 WorkBuddy 接口。

## 本地数据

| 数据 | 默认位置 |
| --- | --- |
| 宏文件和保存的录制 | `宏/` |
| 自动化流程 | `自动化脚本/` |
| 全局设置、绑定方案、宏组库和 AI 连接设置 | 项目根目录下各自的 `webui_*.json` |
| WebView 存储 | `appdata/webview/` |

录制期间请避开密码和其他敏感输入。分享宏或自动化脚本前，可检查其中的文字、文件路径、窗口名称和录制内容。

## 来源与许可

本项目基于 Masclrin 的 MacroDriver WebUI，公开发布待确认上游再发布授权和适用许可证。[上游许可证说明](https://github.com/Masclrin/MacroDriver-WebUI/blob/716160d3cd25631dadd788a49045651620e73883/README.md)

自动化编辑器使用 Font Awesome Free 6.5.0 的本地样式和字库。组件附带其[许可证](webui/vendor/fontawesome/LICENSE.txt)。
