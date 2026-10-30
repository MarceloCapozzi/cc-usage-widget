@echo off
rem cc-usage publisher for Windows.
rem   setup.cmd                    install, then verify (double-click this file)
rem   setup.cmd uninstall          remove it; add -Purge to also remove the secrets
rem   setup.cmd rotate-key         replace this machine's identity (its key pair),
rem                                then verify; asks for the alias, or add
rem                                -ConfirmAlias <alias> (Install.ps1 -RotateKey)
rem Extra arguments go to Install.ps1 (or Uninstall.ps1), e.g. setup.cmd -Force,
rem and after an install to Verify.ps1 too, so it checks the install just made
rem (-Prefix, -ClaudeSettings, -TaskName); it ignores Install-only options.
rem Runs Windows PowerShell with -ExecutionPolicy Bypass for these scripts only;
rem nothing needs administrator rights. Set CCU_NO_PAUSE=1 to skip the pause.
setlocal
set "CCU_DIR=%~dp0"
set "CCU_PS=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%CCU_PS%" set "CCU_PS=powershell.exe"

rem Integrity first: every file must match SHA256SUMS before anything in the
rem bundle is unblocked or run (Install.ps1 checks again). This is a command,
rem not a script, so the execution policy does not stop it. Exit codes: 0 every
rem file matches; 1 a file differs or is missing; 2 no SHA256SUMS (the installed
rem copy of these scripts has none, and nothing there needs unblocking); 3
rem PowerShell is not in FullLanguage mode (the scripts then say why they stop).
set "CCU_CHECK=$ErrorActionPreference = 'Stop'; if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { exit 3 };"
set "CCU_CHECK=%CCU_CHECK% try { $d = $env:CCU_DIR; $s = [IO.Path]::Combine($d, 'SHA256SUMS'); if (-not [IO.File]::Exists($s)) { exit 2 }; $bad = 0; $n = 0;"
set "CCU_CHECK=%CCU_CHECK% foreach ($l in [IO.File]::ReadAllLines($s)) { if (-not $l.Trim()) { continue }; if ($l -notmatch '^([0-9a-f]{64})  (.+)$') { Write-Host ('unreadable line in SHA256SUMS: ' + $l); $bad++; continue };"
set "CCU_CHECK=%CCU_CHECK% $n++; $want = $Matches[1]; $rel = $Matches[2]; $p = [IO.Path]::Combine($d, $rel.Replace('/', '\')); if (-not [IO.File]::Exists($p)) { Write-Host ('missing: ' + $rel); $bad++; continue };"
set "CCU_CHECK=%CCU_CHECK% $sha = [Security.Cryptography.SHA256]::Create(); $fs = [IO.File]::OpenRead($p); try { $h = [BitConverter]::ToString($sha.ComputeHash($fs)).Replace('-', '').ToLowerInvariant() } finally { $fs.Dispose(); $sha.Dispose() };"
set "CCU_CHECK=%CCU_CHECK% if ($h -ne $want) { Write-Host ('differs: ' + $rel); $bad++ } }; if ($n -eq 0) { Write-Host 'SHA256SUMS lists no files'; $bad++ }; if ($bad -gt 0) { exit 1 }; exit 0 } catch { Write-Host $_.Exception.Message; exit 1 }"
"%CCU_PS%" -NoProfile -NonInteractive -Command "%CCU_CHECK%"
set "CCU_SUMS=%ERRORLEVEL%"
set "CCU_CHECK="
if "%CCU_SUMS%"=="0" goto unblock
if "%CCU_SUMS%"=="2" goto run
if "%CCU_SUMS%"=="3" goto run
echo.
echo ERROR: the bundle does not match its SHA256SUMS (exit code %CCU_SUMS%), so nothing in it was unblocked or run.
echo Copy the bundle again. To uninstall, run setup.cmd uninstall from %LOCALAPPDATA%\cc-usage\bundle instead.
set "CCU_RC=1"
goto done

:unblock
rem Files from a download or a zip carry the Mark of the Web. Clearing it is a
rem command, not a script, so the execution policy does not stop it.
"%CCU_PS%" -NoProfile -NonInteractive -Command "Get-ChildItem -LiteralPath $env:CCU_DIR -Recurse -File | Unblock-File" >nul 2>&1

:run
if /i "%~1"=="uninstall" goto uninstall
if /i "%~1"=="rotate-key" goto rotate

"%CCU_PS%" -NoProfile -ExecutionPolicy Bypass -File "%CCU_DIR%Install.ps1" %*
set "CCU_RC=%ERRORLEVEL%"
if not "%CCU_RC%"=="0" goto done
echo.
"%CCU_PS%" -NoProfile -ExecutionPolicy Bypass -File "%CCU_DIR%Verify.ps1" %*
set "CCU_RC=%ERRORLEVEL%"
goto done

:uninstall
"%CCU_PS%" -NoProfile -ExecutionPolicy Bypass -File "%CCU_DIR%Uninstall.ps1" %2 %3 %4 %5 %6 %7 %8 %9
set "CCU_RC=%ERRORLEVEL%"
goto done

:rotate
"%CCU_PS%" -NoProfile -ExecutionPolicy Bypass -File "%CCU_DIR%Install.ps1" -RotateKey %2 %3 %4 %5 %6 %7 %8 %9
set "CCU_RC=%ERRORLEVEL%"
if not "%CCU_RC%"=="0" goto done
echo.
"%CCU_PS%" -NoProfile -ExecutionPolicy Bypass -File "%CCU_DIR%Verify.ps1" %2 %3 %4 %5 %6 %7 %8 %9
set "CCU_RC=%ERRORLEVEL%"

:done
echo.
if "%CCU_RC%"=="0" (echo Done.) else (echo Finished with errors, exit code %CCU_RC%. See the messages above.)
if not defined CCU_NO_PAUSE pause
exit /b %CCU_RC%
