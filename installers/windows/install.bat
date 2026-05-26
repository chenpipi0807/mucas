@echo off
setlocal EnableDelayedExpansion
chcp 65001 >nul 2>&1

echo.
echo   muCAS Installer
echo   ===============
echo.

set "DEST=%LOCALAPPDATA%\Programs\mucas"
set "SCRIPT_DIR=%~dp0"

if not exist "%SCRIPT_DIR%mucas.exe" (
    echo   ERROR: mucas.exe not found next to install.bat
    echo   Please extract the full zip archive before running this installer.
    echo.
    pause
    exit /b 1
)

:: Create install directory and copy files
mkdir "%DEST%" 2>nul
copy /Y "%SCRIPT_DIR%mucas.exe" "%DEST%\mucas.exe" >nul
if exist "%SCRIPT_DIR%mucas.ico" copy /Y "%SCRIPT_DIR%mucas.ico" "%DEST%\mucas.ico" >nul

:: Add to user PATH via PowerShell (idempotent)
powershell.exe -NoProfile -Command ^
  "$dest='%DEST%'; $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not ($p -split ';' | Where-Object{$_ -eq $dest})){[Environment]::SetEnvironmentVariable('Path',($p+';'+$dest).TrimStart(';'),'User')}" >nul 2>&1

:: Write all registry entries via PowerShell (avoids cmd.exe quoting nightmares)
powershell.exe -NoProfile -Command ^
  "$dest='%DEST%'; ^
   $exe = $dest + '\mucas.exe'; ^
   $ico = $dest + '\mucas.ico'; ^
   $cmd_pack   = 'cmd /k \"\"' + $exe + '\" pack \"%1\"\"'; ^
   $cmd_unpack = 'cmd /k \"\"' + $exe + '\" unpack \"%1\"\"'; ^
   $cmd_list   = 'cmd /k \"\"' + $exe + '\" list \"%1\"\"'; ^
   New-Item -Path 'HKCU:\Software\Classes\.mcar' -Force | Set-ItemProperty -Name '(Default)' -Value 'mucas.archive'; ^
   New-Item -Path 'HKCU:\Software\Classes\mucas.archive' -Force | Set-ItemProperty -Name '(Default)' -Value 'muCAS Archive'; ^
   if(Test-Path $ico){New-Item -Path 'HKCU:\Software\Classes\mucas.archive\DefaultIcon' -Force | Set-ItemProperty -Name '(Default)' -Value $ico}; ^
   New-Item -Path 'HKCU:\Software\Classes\mucas.archive\shell\open' -Force | Set-ItemProperty -Name '(Default)' -Value 'Unpack here'; ^
   New-Item -Path 'HKCU:\Software\Classes\mucas.archive\shell\open\command' -Force | Set-ItemProperty -Name '(Default)' -Value $cmd_unpack; ^
   New-Item -Path 'HKCU:\Software\Classes\mucas.archive\shell\list' -Force | Set-ItemProperty -Name '(Default)' -Value 'List contents'; ^
   New-Item -Path 'HKCU:\Software\Classes\mucas.archive\shell\list\command' -Force | Set-ItemProperty -Name '(Default)' -Value $cmd_list; ^
   New-Item -Path 'HKCU:\Software\Classes\Directory\shell\MuCASPack' -Force | Set-ItemProperty -Name '(Default)' -Value 'Pack with muCAS'; ^
   if(Test-Path $ico){Set-ItemProperty -Path 'HKCU:\Software\Classes\Directory\shell\MuCASPack' -Name 'Icon' -Value $ico}; ^
   New-Item -Path 'HKCU:\Software\Classes\Directory\shell\MuCASPack\command' -Force | Set-ItemProperty -Name '(Default)' -Value $cmd_pack; ^
   Write-Host 'Registry OK'"

if errorlevel 1 (
    echo   ERROR: Registry setup failed. Is PowerShell available?
    pause
    exit /b 1
)

:: Refresh PATH for current session
set "PATH=%PATH%;%DEST%"

echo.
echo   Installed to:  %DEST%
echo.
echo   Right-click any folder   -^>  "Pack with muCAS"
echo   Right-click any .mcar    -^>  "Unpack here" / "List contents"
echo.
echo   TIP: If menu items don't appear, restart File Explorer:
echo        Task Manager (Ctrl+Shift+Esc) -^> Details -^> explorer.exe -^> End task
echo        Then: File -^> Run new task -^> explorer.exe
echo.
pause
