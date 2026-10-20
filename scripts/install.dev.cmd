@echo off
setlocal enableextensions
title CloudFile Agent 2.2 安装（测试通道）

rem ---------------------------------------------------------------
rem  双击运行即可。本脚本只做一件事：拿到引导体，然后交给它。
rem    1) 同目录有 cloudfile-install.exe 就直接用（推荐：离线也能装）；
rem    2) 没有就从通道下载：主源是门户域名，备用源是直连静态服务 IP。
rem  验签、下载 Agent、注册 Native Host、写自启、启动桌面端全在引导体里，
rem  它内置钉住的发布公钥 —— 通道只能提供文件，不能决定「谁签发」。
rem
rem  正式与测试只差下面 CHANNEL 一行，二进制是同一份：
rem    正式 cloudfile-updates      -> install.cmd
rem    测试 cloudfile-updates-dev  -> install.dev.cmd
rem
rem  安装目录：%CLOUDFILE_AGENT2_HOME% 优先，否则 %LOCALAPPDATA%\CloudFileAgent22。
rem  自动化调用时设 CLOUDFILE_NO_PAUSE=1 可跳过结尾的暂停。
rem  注意：echo 的文本里不要出现 > < & |，cmd 会把它们当重定向或管道。
rem ---------------------------------------------------------------

set "CHANNEL=cloudfile-updates-dev"

rem Deployment must set release origins explicitly.
set "BASE_PRIMARY=%CLOUDFILE_INSTALL_SOURCE%"
set "BASE_MIRROR=%CLOUDFILE_INSTALL_FALLBACK%"
if not defined BASE_PRIMARY (
  echo [ERROR] CLOUDFILE_INSTALL_SOURCE is not set.
  echo Set it to the signed release channel URL before running this installer.
  exit /b 1
)

set "INSTALLER=cloudfile-install.exe"
set "WORKDIR=%LOCALAPPDATA%\CloudFileAgent"
rem 有 D 盘就落 D 盘，没有才回到系统盘
if exist "D:\" set "WORKDIR=D:\CloudFileAgent"
if not "%CLOUDFILE_AGENT2_HOME%"=="" set "WORKDIR=%CLOUDFILE_AGENT2_HOME%"
if not exist "%WORKDIR%" mkdir "%WORKDIR%" >nul 2>&1

set "TARGET=%~dp0%INSTALLER%"
if exist "%TARGET%" goto run
set "TARGET=%WORKDIR%\%INSTALLER%"
if exist "%TARGET%" (
  rem Cached executables outside the supplied bundle also require publisher verification.
  if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
    echo [ERROR] Cached installer requires CLOUDFILE_INSTALL_BOOTSTRAP_SHA256.
    set "CODE=1"
    goto end
  )
  goto run
)

rem A network-downloaded executable is never a trusted bootstrap on size alone.
rem Supply its publisher-pinned SHA-256, or ship cloudfile-install.exe alongside this CMD.
if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
  echo [ERROR] Cannot download a bootstrap EXE without a pinned SHA-256.
  echo Put cloudfile-install.exe next to this script, or set CLOUDFILE_INSTALL_BOOTSTRAP_SHA256.
  set "CODE=1"
  goto end
)

echo 正在获取安装引导体...
call :fetch "%BASE_PRIMARY%/%INSTALLER%"
if not errorlevel 1 goto run
if defined BASE_MIRROR call :fetch "%BASE_MIRROR%/%INSTALLER%"
if not errorlevel 1 goto run

echo.
echo [失败] 两个地址都没能下载 %INSTALLER%。
echo        请检查本机能否访问下面的地址（DNS / 代理 / 防火墙）：
echo          %BASE_PRIMARY%
echo          %BASE_MIRROR%
echo        也可以把 %INSTALLER% 与本脚本放在同一目录后重试。
set "CODE=1"
goto end

:run
rem If the publisher supplied a hash, verify both cached and downloaded EXEs.
if defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
  call :verify
  if errorlevel 1 (
    echo [ERROR] Bootstrap SHA-256 verification failed. Refusing to execute it.
    set "CODE=1"
    goto end
  )
)
echo 通道：%BASE_PRIMARY%
if defined BASE_MIRROR (
  "%TARGET%" --source "%BASE_PRIMARY%" --source-fallback "%BASE_MIRROR%" %*
) else (
  "%TARGET%" --source "%BASE_PRIMARY%" %*
)
set "CODE=%ERRORLEVEL%"
goto end

:fetch
rem Both primary and fallback must match the same publisher-pinned digest.
set "CF_INSTALL_FETCH_URL=%~1"
set "CF_INSTALL_FETCH_TARGET=%TARGET%"
powershell -NoProfile -Command "$ErrorActionPreference='Stop'; try { $want=$env:CLOUDFILE_INSTALL_BOOTSTRAP_SHA256; if ($want -notmatch '^[a-fA-F0-9]{64}$') { exit 2 }; Invoke-WebRequest -UseBasicParsing -TimeoutSec 60 -Uri $env:CF_INSTALL_FETCH_URL -OutFile $env:CF_INSTALL_FETCH_TARGET; $got=(Get-FileHash -Algorithm SHA256 -LiteralPath $env:CF_INSTALL_FETCH_TARGET).Hash; if ($got -ine $want) { Remove-Item -LiteralPath $env:CF_INSTALL_FETCH_TARGET -Force -ErrorAction SilentlyContinue; exit 3 } } catch { Remove-Item -LiteralPath $env:CF_INSTALL_FETCH_TARGET -Force -ErrorAction SilentlyContinue; exit 1 }"
exit /b %ERRORLEVEL%

:verify
set "CF_INSTALL_FETCH_TARGET=%TARGET%"
powershell -NoProfile -Command "$ErrorActionPreference='Stop'; try { $want=$env:CLOUDFILE_INSTALL_BOOTSTRAP_SHA256; if ($want -notmatch '^[a-fA-F0-9]{64}$') { exit 2 }; $got=(Get-FileHash -Algorithm SHA256 -LiteralPath $env:CF_INSTALL_FETCH_TARGET).Hash; if ($got -ine $want) { exit 3 } } catch { exit 1 }"
exit /b %ERRORLEVEL%

:end
if not defined CODE set "CODE=1"
echo.
echo 退出码：%CODE%
if not "%CLOUDFILE_NO_PAUSE%"=="" exit /b %CODE%
pause
exit /b %CODE%
