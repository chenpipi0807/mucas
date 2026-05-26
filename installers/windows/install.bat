@echo off
setlocal EnableDelayedExpansion
chcp 65001 >nul 2>&1

echo.
echo   μCAS Installer
echo   ==============
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

:: Create install directory
mkdir "%DEST%" 2>nul

:: Copy files
copy /Y "%SCRIPT_DIR%mucas.exe" "%DEST%\mucas.exe" >nul
if exist "%SCRIPT_DIR%mucas.ico" copy /Y "%SCRIPT_DIR%mucas.ico" "%DEST%\mucas.ico" >nul

:: Add to user PATH (idempotent — only adds if not already present)
set "UPATH="
for /f "skip=2 tokens=2,*" %%A in ('reg query "HKCU\Environment" /v Path 2^>nul') do set "UPATH=%%B"
echo !UPATH! | findstr /i /c:"%DEST%" >nul 2>&1
if errorlevel 1 (
    if "!UPATH!"=="" (
        reg add "HKCU\Environment" /v Path /t REG_EXPAND_SZ /d "%DEST%" /f >nul
    ) else (
        reg add "HKCU\Environment" /v Path /t REG_EXPAND_SZ /d "!UPATH!;%DEST%" /f >nul
    )
)

:: .mcar file association and type registration
reg add "HKCU\Software\Classes\.mcar"          /ve /d "mucas.archive" /f >nul
reg add "HKCU\Software\Classes\mucas.archive"  /ve /d "μCAS Archive"  /f >nul
if exist "%DEST%\mucas.ico" (
    reg add "HKCU\Software\Classes\mucas.archive\DefaultIcon" /ve /d "%DEST%\mucas.ico" /f >nul
)

:: Context menu on .mcar files: Unpack here
reg add "HKCU\Software\Classes\mucas.archive\shell\open"         /ve /d "Unpack here"    /f >nul
reg add "HKCU\Software\Classes\mucas.archive\shell\open\command" /ve ^
    /d "cmd /k \"set PROMPT=[$P] $$ ^\& \"%DEST%\mucas.exe\" unpack \"%1\"\"" /f >nul

:: Context menu on .mcar files: List contents
reg add "HKCU\Software\Classes\mucas.archive\shell\list"         /ve /d "List contents"  /f >nul
reg add "HKCU\Software\Classes\mucas.archive\shell\list\command" /ve ^
    /d "cmd /k \"set PROMPT=[$P] $$ ^\& \"%DEST%\mucas.exe\" list \"%1\"\"" /f >nul

:: Context menu on folders: Pack with μCAS
reg add "HKCU\Software\Classes\Directory\shell\MuCASPack"         /ve /d "Pack with μCAS" /f >nul
if exist "%DEST%\mucas.ico" (
    reg add "HKCU\Software\Classes\Directory\shell\MuCASPack" /v Icon /d "%DEST%\mucas.ico" /f >nul
)
reg add "HKCU\Software\Classes\Directory\shell\MuCASPack\command" /ve ^
    /d "cmd /k \"set PROMPT=[$P] $$ ^\& \"%DEST%\mucas.exe\" pack \"%1\"\"" /f >nul

:: Broadcast environment change so Explorer picks up PATH without reboot
powershell -NoProfile -Command ^
    "[System.Environment]::SetEnvironmentVariable('Path',[System.Environment]::GetEnvironmentVariable('Path','User'),'User')" >nul 2>&1

:: Refresh shell env for current session too
set "PATH=%PATH%;%DEST%"

echo   Installed to:  %DEST%
echo.
echo   Right-click any folder    ->  "Pack with μCAS"
echo   Right-click any .mcar     ->  "Unpack here" / "List contents"
echo.
echo   TIP: Restart File Explorer if menu items don't appear yet.
echo        (Right-click taskbar -> Task Manager -> Restart Windows Explorer)
echo.
pause
