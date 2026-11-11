@echo off
setlocal enableextensions
title CloudFile Agent 安装（测试通道）

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
  echo [错误] 没有找到安装源，无法继续。
  echo        请使用管理员提供的完整安装包，不要单独复制本文件。
  exit /b 1
)

set "INSTALLER=cloudfile-install.exe"
set "WORKDIR=%LOCALAPPDATA%\CloudFileAgent"
rem 有 D 盘就落 D 盘，没有才回到系统盘
if exist "D:\" set "WORKDIR=D:\CloudFileAgent"
if not "%CLOUDFILE_AGENT2_HOME%"=="" set "WORKDIR=%CLOUDFILE_AGENT2_HOME%"
if not exist "%WORKDIR%" mkdir "%WORKDIR%" >nul 2>&1

rem 现场排查用：只验证"能不能下载到引导体并通过哈希校验"，不执行安装、不碰系统。
rem 用法：install.cmd --fetch-only
rem 想模拟"PATH 被裁剪"的机器：cmd /c "set PATH=&& install.cmd --fetch-only"
if /i "%~1"=="--fetch-only" goto fetch_only

set "TARGET=%~dp0%INSTALLER%"
if exist "%TARGET%" (
  set "SOURCE_KIND=bundle"
  goto run
)
set "TARGET=%WORKDIR%\%INSTALLER%"
if exist "%TARGET%" (
  rem Cached executables outside the supplied bundle also require publisher verification.
  if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
    echo [错误] 本机缓存的安装组件缺少校验信息，无法确认它是否被篡改。
    set "CODE=1"
    goto end
  )
  set "SOURCE_KIND=cache"
  goto run
)

rem A network-downloaded executable is never a trusted bootstrap on size alone.
rem Supply its publisher-pinned SHA-256, or ship cloudfile-install.exe alongside this CMD.
if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
  echo [错误] 缺少安装组件的校验信息，为安全起见已停止。
  echo        请把 %INSTALLER% 与本脚本放在同一目录后重试。
  set "CODE=1"
  goto end
)

echo 正在获取安装引导体...
call :fetch "%BASE_PRIMARY%/%INSTALLER%"
if not errorlevel 1 goto run
if defined BASE_MIRROR call :fetch "%BASE_MIRROR%/%INSTALLER%"
if not errorlevel 1 goto run

echo.
echo [失败] 下载安装组件失败。
echo        请确认本机能正常访问公司网络（网络、代理、防火墙）后重试。
echo        多次失败的话，把下面两行发给管理员：
echo          %BASE_PRIMARY%
echo          %BASE_MIRROR%
set "CODE=1"
goto end

:run
rem If the publisher supplied a hash, verify both cached and downloaded EXEs.
if defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
  call :verify
  if errorlevel 1 (
    rem 本机缓存的是上一次安装留下的引导体（通道换了新版就必然对不上）。
    rem 缓存不可信但**可以换掉**：删掉后按钉住的哈希重新下载，仍然逐字节校验。
    rem 只有「和脚本放在一起的发布包」对不上才直接拒绝 —— 那是发布物本身有问题。
    if /i "%SOURCE_KIND%"=="cache" (
      echo [提示] 本机缓存的安装组件与当前安装源不一致，正在重新下载...
      del /f /q "%TARGET%" >nul 2>&1
      call :fetch "%BASE_PRIMARY%/%INSTALLER%"
      if errorlevel 1 if defined BASE_MIRROR call :fetch "%BASE_MIRROR%/%INSTALLER%"
      if errorlevel 1 (
        echo [错误] 安装组件下载失败或校验不通过，已停止。
        set "CODE=1"
        goto end
      )
    ) else (
      echo [错误] 安装组件校验不通过，已停止。
      echo         请重新下载完整安装包后重试；脚本旁边的 %INSTALLER% 与官方发布的不一致。
      set "CODE=1"
      goto end
    )
  )
)
echo 安装源：%BASE_PRIMARY%
if defined BASE_MIRROR (
  "%TARGET%" --source "%BASE_PRIMARY%" --source-fallback "%BASE_MIRROR%" %*
) else (
  "%TARGET%" --source "%BASE_PRIMARY%" %*
)
set "CODE=%ERRORLEVEL%"
goto end

:fetch_only
set "TARGET=%WORKDIR%\cloudfile-install.test.exe"
if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 (
  echo [TEST] 没有内置的引导体哈希，无法校验；请用发布包里带签名的那份脚本。
  set "CODE=1"
  goto end
)
echo [TEST] 只测试下载与校验，不安装。目标：%TARGET%
echo [TEST] 主源：%BASE_PRIMARY%
call :fetch "%BASE_PRIMARY%/%INSTALLER%"
set "CODE=%ERRORLEVEL%"
if not "%CODE%"=="0" if defined BASE_MIRROR (
  echo [TEST] 主源失败（退出码 %CODE%），改试备用源：%BASE_MIRROR%
  call :fetch "%BASE_MIRROR%/%INSTALLER%"
  set "CODE=%ERRORLEVEL%"
)
if "%CODE%"=="0" (
  echo [OK] 下载完成，校验通过：%TARGET%
) else (
  echo [FAIL] 下载或校验失败，退出码 %CODE%。
  echo        本机可用的下载方式都试过了，均未成功。
)
del /f /q "%TARGET%" >nul 2>&1
rem 测试模式到此为止：不能落到「安装完成」的横幅，那会把自检结果说成装好了。
if not "%CLOUDFILE_NO_PAUSE%"=="" exit /b %CODE%
pause
exit /b %CODE%

:fetch
rem Both primary and fallback must match the same publisher-pinned digest.
set "CF_INSTALL_FETCH_URL=%~1"
set "CF_INSTALL_FETCH_TARGET=%TARGET%"
set "CF_FETCHED="
del /f /q "%TARGET%" >nul 2>&1
rem 修改逻辑（2026-11-09）：原来只靠 %CF_PS% 下载。现场机器的 PATH 里根本没有它
rem （报"不是内部或外部命令"），主源和备源都拿不到引导体，安装直接失败。
rem 现在**先用绝对路径调用系统自带的下载器** —— PATH 被裁剪时也能用：
rem   %SystemRoot%\System32\curl.exe      Win10 1803+
rem   %SystemRoot%\System32\certutil.exe  所有 Windows 都有
rem 两者都不在才退回按名字找，最后才是原来那个脚本宿主（纯兼容兜底）。
set "CF_CURL=%SystemRoot%\System32\curl.exe"
if not exist "%CF_CURL%" set "CF_CURL=curl.exe"
set "CF_CERTUTIL=%SystemRoot%\System32\certutil.exe"
if not exist "%CF_CERTUTIL%" set "CF_CERTUTIL=certutil.exe"
set "CF_PS=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%CF_PS%" set "CF_PS=powershell.exe"
"%CF_CURL%" -fsSL --max-time 60 --retry 1 -o "%TARGET%" "%CF_INSTALL_FETCH_URL%" >nul 2>&1
if not errorlevel 1 if exist "%TARGET%" set "CF_FETCHED=1"
if not defined CF_FETCHED del /f /q "%TARGET%" >nul 2>&1
if not defined CF_FETCHED (
  "%CF_CERTUTIL%" -urlcache -split -f "%CF_INSTALL_FETCH_URL%" "%TARGET%" >nul 2>&1
  if not errorlevel 1 if exist "%TARGET%" set "CF_FETCHED=1"
  if not defined CF_FETCHED del /f /q "%TARGET%" >nul 2>&1
)
if not defined CF_FETCHED (
  "%CF_PS%" -NoProfile -Command "$ErrorActionPreference='Stop'; try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 60 -Uri $env:CF_INSTALL_FETCH_URL -OutFile $env:CF_INSTALL_FETCH_TARGET } catch { Remove-Item -LiteralPath $env:CF_INSTALL_FETCH_TARGET -Force -ErrorAction SilentlyContinue; exit 1 }" >nul 2>&1
  if not errorlevel 1 if exist "%TARGET%" set "CF_FETCHED=1"
)
if not defined CF_FETCHED exit /b 1
rem 不管谁下的，统一在这里按钉住的哈希校验一次（校验也不再依赖那个脚本宿主）。
call :hash_file "%TARGET%"
if errorlevel 1 (
  del /f /q "%TARGET%" >nul 2>&1
  exit /b 1
)
if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 exit /b 0
if /i not "%CF_HASH%"=="%CLOUDFILE_INSTALL_BOOTSTRAP_SHA256%" (
  del /f /q "%TARGET%" >nul 2>&1
  exit /b 3
)
exit /b 0

:verify
if not exist "%TARGET%" exit /b 1
call :hash_file "%TARGET%"
if errorlevel 1 exit /b 1
if not defined CLOUDFILE_INSTALL_BOOTSTRAP_SHA256 exit /b 0
if /i not "%CF_HASH%"=="%CLOUDFILE_INSTALL_BOOTSTRAP_SHA256%" exit /b 3
exit /b 0

:hash_file
rem 用 certutil 算 SHA256，再用 cmd 内建的 type 读出来解析。
rem 修改逻辑（2026-11-09）：原来靠脚本宿主 Get-FileHash，后来改用管道 + findstr，
rem 但现场有机器 PATH 被裁剪成空 —— 管道里带引号的程序路径会报"文件名、目录名或
rem 卷标语法不正确"，findstr 也找不到。现在只用绝对路径 + 内建命令，PATH 无关。
set "CF_HASH="
set "CF_CERTUTIL=%SystemRoot%\System32\certutil.exe"
if not exist "%CF_CERTUTIL%" set "CF_CERTUTIL=certutil.exe"
set "CF_HASH_TMP=%TEMP%\cf-hash-%RANDOM%%RANDOM%.txt"
if not defined TEMP set "CF_HASH_TMP=%~dp0cf-hash.tmp"
"%CF_CERTUTIL%" -hashfile "%~1" SHA256 > "%CF_HASH_TMP%" 2>nul
if errorlevel 1 (
  del /f /q "%CF_HASH_TMP%" >nul 2>&1
  exit /b 1
)
for /f "delims=" %%H in ('type "%CF_HASH_TMP%"') do call :pick_hash "%%H"
del /f /q "%CF_HASH_TMP%" >nul 2>&1
if not defined CF_HASH exit /b 1
exit /b 0

:pick_hash
rem 第一行是说明文字（随语言变化），真正的摘要是"去掉空格后正好 64 个十六进制字符"
rem 的那一行 —— 按形状认，不解析任何本地化文案。
if defined CF_HASH exit /b 0
set "CF_LINE=%~1"
set "CF_LINE=%CF_LINE: =%"
if "%CF_LINE:~63,1%"=="" exit /b 0
if not "%CF_LINE:~64,1%"=="" exit /b 0
set "CF_BAD="
for /f "delims=0123456789abcdefABCDEF" %%X in ("%CF_LINE%") do set "CF_BAD=1"
if defined CF_BAD exit /b 0
set "CF_HASH=%CF_LINE%"
exit /b 0

:end
if not defined CODE set "CODE=1"
echo.
if "%CODE%"=="0" (
  echo [OK] 安装完成，相关组件已装好。
  echo      任务中心会在几秒内自动出现；如果没有，重新运行一次本脚本即可。
  echo      如果提示缺少 WebView2 运行时，按提示装好后再运行一次本脚本。
) else (
  echo [错误] 安装没有完成（错误码 %CODE%）。
  echo         请把上面的提示拍照发给管理员；多数情况下重新运行一次本脚本就能装好。
)
if not "%CLOUDFILE_NO_PAUSE%"=="" exit /b %CODE%
pause
exit /b %CODE%
