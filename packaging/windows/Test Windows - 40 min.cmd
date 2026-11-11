@echo off
title VMA - test Windows
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0bin\launch.ps1" -Mode Test
