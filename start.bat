@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0" || exit /b 1

if not exist "%~dp0superfaye-macro.exe" (
    call "%~dp0build.bat"
    if errorlevel 1 exit /b 1
)

rem Stage executable, WebView2Loader & shortcut icon into ASCII temp/appdata paths for Windows compatibility.
set "MACRO_BIN_DIR=%TEMP%\SuperFayeMacro-Rust"
if not exist "%MACRO_BIN_DIR%" mkdir "%MACRO_BIN_DIR%" >nul 2>&1

xcopy /d /y /q "%~dp0superfaye-macro.exe" "%MACRO_BIN_DIR%\" >nul || exit /b 1
if exist "%~dp0WebView2Loader.dll" xcopy /d /y /q "%~dp0WebView2Loader.dll" "%MACRO_BIN_DIR%\" >nul
if defined LOCALAPPDATA if exist "%~dp0webui\assets\icon.ico" (
    if not exist "%LOCALAPPDATA%\MacroDriver-Rust" mkdir "%LOCALAPPDATA%\MacroDriver-Rust" >nul 2>&1
    xcopy /d /y /q "%~dp0webui\assets\icon.ico" "%LOCALAPPDATA%\MacroDriver-Rust\" >nul
)

set "MACRO_PROJECT_ROOT=%~dp0"
if /i "%~1"=="--check" (
    "%MACRO_BIN_DIR%\superfaye-macro.exe" --smoke-health
    exit /b %errorlevel%
)

echo [启动] 正在打开 SuperFaye Macro (Rust)...
start "" /d "%~dp0" "%MACRO_BIN_DIR%\superfaye-macro.exe" %*