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

powershell.exe -NoProfile -NonInteractive -Command "$ErrorActionPreference = 'Stop'; $t = ((cargo metadata --no-deps --format-version 1) -join '' | ConvertFrom-Json).target_directory; $rel = Join-Path $t 'release'; $exe = '.\superfaye-macro.exe'; Copy-Item -LiteralPath (Join-Path $rel 'superfaye-macro.exe') -Destination $exe -Force; $loader = Join-Path $rel 'WebView2Loader.dll'; $installed = Join-Path (Get-Location) 'WebView2Loader.dll'; if ((Test-Path -LiteralPath $loader) -and ((-not (Test-Path -LiteralPath $installed)) -or ([Convert]::ToBase64String([IO.File]::ReadAllBytes($loader)) -cne [Convert]::ToBase64String([IO.File]::ReadAllBytes($installed))))) { Copy-Item -LiteralPath $loader -Destination $installed -Force }; (Get-Item -LiteralPath $exe).LastWriteTimeUtc = [DateTime]::UtcNow"
if errorlevel 1 (
    echo [错误] 复制产物失败。
    exit /b 1
)

echo [完成] 已输出 superfaye-macro.exe
