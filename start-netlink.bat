@echo off
cd /d "%~dp0"
title NetLink Launcher

echo [1/3] Starting vite dev server (localhost:5173)...
start "NetLink-vite" /min cmd /c "cd web && pnpm run dev"

echo [2/3] Waiting for vite to be ready (up to 80s)...
set /a tries=0
:wait_vite
netstat -ano | findstr ":5173 " | findstr "LISTENING" >nul
if %errorlevel%==0 goto vite_ok
set /a tries+=1
if %tries% geq 40 (
    echo   vite not ready in time, starting App anyway...
    goto run_app
)
%SystemRoot%\System32\timeout.exe /t 2 /nobreak >nul
goto wait_vite
:vite_ok
echo   vite is ready.

:run_app
echo [3/3] Starting NetLink App - close the App window to stop everything...
set "RUSTUP_HOME=E:\rust-toolchains"
set "CARGO_HOME=E:\rust-cache"
set "PATH=E:\rust-cache\bin;%PATH%"
cargo run --no-default-features -p netlink-app

echo App exited, cleaning up vite...
for /f "tokens=5" %%a in ('netstat -ano ^| findstr ":5173 " ^| findstr "LISTENING"') do taskkill /F /T /PID %%a >nul 2>&1
exit /b 0
