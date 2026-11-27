@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0" || exit /b 1

rem Rebuild when the project exe is missing or older than sources, so --check
rem and launches do not stage a stale binary. The check is read-only; PATH is
rem only adjusted inside build.bat's own process if a rebuild is needed.
set "NEED_BUILD=0"
if not exist "%~dp0superfaye-macro.exe" (
    set "NEED_BUILD=1"
) else (
    set "MACRO_CHECK_ROOT=%~dp0"
    powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$root = $env:MACRO_CHECK_ROOT.TrimEnd('\'); $exe = Join-Path $root 'superfaye-macro.exe'; $t = (Get-Item -LiteralPath $exe).LastWriteTime; $watch = @(); foreach ($d in @('src','webui','assets')) { $p = Join-Path $root $d; if (Test-Path -LiteralPath $p) { $watch += Get-ChildItem -LiteralPath $p -Recurse -File -ErrorAction SilentlyContinue } }; $watch += Get-ChildItem -LiteralPath $root -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -in @('Cargo.toml','Cargo.lock','build.rs','build.bat','rust-toolchain.toml') }; if ($watch | Where-Object { $_.FullName -notmatch '\\webui\\vendor\\' -and $_.LastWriteTime -gt $t } | Select-Object -First 1) { exit 1 }; exit 0"
    if errorlevel 1 set "NEED_BUILD=1"
)
if "%NEED_BUILD%"=="1" (
    call "%~dp0build.bat"
    if errorlevel 1 exit /b 1
)

rem Stage executable, WebView2Loader & shortcut icon into ASCII temp/appdata paths for Windows compatibility.
set "MACRO_BIN_DIR=%TEMP%\SuperFayeMacro-Rust"
if not exist "%MACRO_BIN_DIR%" mkdir "%MACRO_BIN_DIR%" >nul 2>&1

rem Force-copy so a newer leftover in TEMP cannot shadow the project exe.
copy /Y "%~dp0superfaye-macro.exe" "%MACRO_BIN_DIR%\superfaye-macro.exe" >nul || exit /b 1
if exist "%~dp0WebView2Loader.dll" copy /Y "%~dp0WebView2Loader.dll" "%MACRO_BIN_DIR%\WebView2Loader.dll" >nul
if defined LOCALAPPDATA if not exist "%~dp0webui\assets\icon-source.png" if exist "%~dp0webui\assets\icon.ico" (
    if not exist "%LOCALAPPDATA%\MacroDriver-Rust" mkdir "%LOCALAPPDATA%\MacroDriver-Rust" >nul 2>&1
    copy /Y "%~dp0webui\assets\icon.ico" "%LOCALAPPDATA%\MacroDriver-Rust\icon.ico" >nul
)

set "MACRO_PROJECT_ROOT=%~dp0"
if /i "%~1"=="--check" (
    "%MACRO_BIN_DIR%\superfaye-macro.exe" --smoke-health
    exit /b %errorlevel%
)

echo [启动] 正在打开 SuperFaye Macro (Rust)...
start "" /d "%~dp0" "%MACRO_BIN_DIR%\superfaye-macro.exe" %*