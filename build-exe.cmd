@echo off
chcp 936 >nul
setlocal
cd /d "%~dp0"
title AI IDE 一键打包
echo ============================================================
echo   AI IDE 一键打包（Windows x64 / NSIS 安装包）
echo   产物：apps\desktop\release\AI-IDE-<版本>-win-x64.exe
echo   使用 D:\nodejs\node.exe 与 pnpm.cjs 显式调用，不依赖 PATH 里的 node
echo ============================================================
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-exe.ps1" %*
set "RC=%ERRORLEVEL%"
chcp 936 >nul
echo.
if not "%RC%"=="0" (
    echo [失败] 打包未完成，退出码 %RC%。请查看上方红色错误信息。
    echo.
    pause
    exit /b %RC%
)
echo [成功] 打包完成，产物见上方路径与大小。
pause
exit /b 0
