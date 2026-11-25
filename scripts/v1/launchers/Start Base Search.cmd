@echo off
setlocal DisableDelayedExpansion
set "program=%~dp0app\base-search.exe"
if not exist "%program%" (
    echo Could not find Base Search. Extract the entire ZIP and try again.
    if not defined CI pause
    exit /b 1
)
start "" "%program%"
exit /b %errorlevel%
