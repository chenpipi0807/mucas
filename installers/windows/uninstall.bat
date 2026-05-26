@echo off
setlocal EnableDelayedExpansion
chcp 65001 >nul 2>&1

echo.
echo   μCAS Uninstaller
echo   ================
echo.

set "DEST=%LOCALAPPDATA%\Programs\mucas"

:: Remove context menus and file associations
reg delete "HKCU\Software\Classes\.mcar"                           /f >nul 2>&1
reg delete "HKCU\Software\Classes\mucas.archive"                   /f >nul 2>&1
reg delete "HKCU\Software\Classes\Directory\shell\MuCASPack"       /f >nul 2>&1

:: Remove from user PATH
set "UPATH="
for /f "skip=2 tokens=2,*" %%A in ('reg query "HKCU\Environment" /v Path 2^>nul') do set "UPATH=%%B"
if not "!UPATH!"=="" (
    set "NEWPATH=!UPATH:%DEST%;=!"
    set "NEWPATH=!NEWPATH:;%DEST%=!"
    set "NEWPATH=!NEWPATH:%DEST%=!"
    if not "!NEWPATH!"=="!UPATH!" (
        reg add "HKCU\Environment" /v Path /t REG_EXPAND_SZ /d "!NEWPATH!" /f >nul
    )
)

:: Remove binary and icon
del /f /q "%DEST%\mucas.exe" >nul 2>&1
del /f /q "%DEST%\mucas.ico" >nul 2>&1
rmdir "%DEST%" >nul 2>&1

echo   μCAS has been uninstalled.
echo.
pause
