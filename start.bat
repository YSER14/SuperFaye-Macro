@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0" || exit /b 1

set "MACRO_REQUIREMENTS=%~dp0bin\requirements.txt"
if not defined PYTHONNET_RUNTIME set "PYTHONNET_RUNTIME=netfx"
rem Use a per-user runtime outside the source checkout.
set "MACRO_VENV="
if not defined LOCALAPPDATA goto failed
for /f "delims=" %%D in ('powershell.exe -NoProfile -NonInteractive -Command "$sha = [Security.Cryptography.SHA256]::Create(); $key = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes((Get-Location).Path))).Replace('-', '').Substring(0, 16); Join-Path $env:LOCALAPPDATA ('SuperFaye-Macro\runtime-' + $key)"') do set "MACRO_VENV=%%D"
if not defined MACRO_VENV goto failed
set "MACRO_PY=%MACRO_VENV%\Scripts\python.exe"

if exist "%MACRO_PY%" (
    "%MACRO_PY%" -c "import ctypes" >nul 2>&1
    if not errorlevel 1 goto check_runtime
)

echo [检查] 正在查找 64 位 Python 3.12...
set "MACRO_BASE_PY="
set "MACRO_BASE_ARGS="
where /q py.exe
if not errorlevel 1 call :probe_python py.exe -3.12
where /q python.exe
if not errorlevel 1 call :probe_python python.exe
where /q python3.exe
if not errorlevel 1 call :probe_python python3.exe

if exist "%LOCALAPPDATA%\Programs\Python\Python312\python.exe" call :probe_python "%LOCALAPPDATA%\Programs\Python\Python312\python.exe"
if exist "%ProgramFiles%\Python312\python.exe" call :probe_python "%ProgramFiles%\Python312\python.exe"
for /d %%D in ("%APPDATA%\uv\python\cpython-3.12-*-windows*" "%APPDATA%\uv\python\cpython-3.12-windows*") do (
    if exist "%%~fD\python.exe" call :probe_python "%%~fD\python.exe"
)
for /d %%D in ("%LOCALAPPDATA%\Python\pythoncore-3.*-64") do (
    if exist "%%~fD\python.exe" call :probe_python "%%~fD\python.exe"
)

if not defined MACRO_BASE_PY (
    echo [错误] 找不到能创建环境的 64 位 Python 3.12。
    goto failed
)
echo [准备] 正在创建项目的 Python 环境...
"%MACRO_BASE_PY%" %MACRO_BASE_ARGS% -m venv --without-pip "%MACRO_VENV%"
if errorlevel 1 goto failed

:check_runtime
call :probe_runtime >nul 2>&1
if errorlevel 1 (
    powershell.exe -NoProfile -NonInteractive -Command "exit [int](New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)"
    if errorlevel 1 (
        echo [错误] 请先用普通权限运行 start.bat --check 安装依赖。
        goto failed
    )
    echo [准备] 正在安装或补齐项目依赖...
    "%MACRO_PY%" -m pip --version >nul 2>&1
    if errorlevel 1 (
        "%MACRO_PY%" -X utf8 -m ensurepip --upgrade --default-pip
        if errorlevel 1 goto failed
    )
    "%MACRO_PY%" -X utf8 -m pip --isolated --disable-pip-version-check install --cache-dir "%MACRO_VENV%\pip-cache" --require-hashes --index-url https://pypi.org/simple -r "%MACRO_REQUIREMENTS%"
    if errorlevel 1 goto failed
    call :probe_runtime
    if errorlevel 1 goto failed
)
echo [通过] 桌面组件和 AI Agent 均可加载。
if /i "%~1"=="--check" exit /b 0

echo [启动] 正在打开宏锻控制台...
set "MACRO_WEBUI_STARTED=1"
"%MACRO_PY%" -E -X utf8 "%~dp0webui_app.py" %*
if errorlevel 1 goto failed
exit /b 0

:probe_python
if defined MACRO_BASE_PY exit /b 0
"%~1" %~2 -c "import ctypes, sys, venv; sys.exit(not (sys.version_info[:2] == (3, 12) and sys.maxsize > 2**32))" >nul 2>&1
if errorlevel 1 exit /b 0
set "MACRO_BASE_PY=%~1"
set "MACRO_BASE_ARGS=%~2"
exit /b 0

:probe_runtime
"%MACRO_PY%" -E -X utf8 -B -c "import cv2, numpy, rapidocr_onnxruntime, webui_app; from webview.platforms import winforms; from automation.assistant_agent import run_editor_agent"
exit /b %errorlevel%

:failed
echo [错误] 启动检查未通过，请查看上方的具体错误。
if /i not "%~1"=="--check" pause
exit /b 1
