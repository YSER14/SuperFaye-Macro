@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0" || exit /b 1

for /d %%D in ("%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT*\mingw64\bin") do (
    set "PATH=%%~fD;%USERPROFILE%\.cargo\bin;%PATH%"
)

echo [构建] 正在编译 SuperFaye Macro Release 版本...
cargo build --release
if errorlevel 1 (
    echo [错误] 编译失败。
    exit /b 1
)

powershell.exe -NoProfile -NonInteractive -Command "$t = ((cargo metadata --no-deps --format-version 1) -join '' | ConvertFrom-Json).target_directory; $rel = Join-Path $t 'release'; Copy-Item (Join-Path $rel 'superfaye-macro.exe') '.\superfaye-macro.exe' -Force; if (Test-Path (Join-Path $rel 'WebView2Loader.dll')) { Copy-Item (Join-Path $rel 'WebView2Loader.dll') '.\WebView2Loader.dll' -Force }"
if errorlevel 1 (
    echo [错误] 复制产物失败。
    exit /b 1
)

echo [完成] 已输出 superfaye-macro.exe
