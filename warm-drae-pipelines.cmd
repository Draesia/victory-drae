@echo off
rem Pre-builds the DRIVER's pipelines for victory-drae, so a run does not stutter (or draw black)
rem while the driver compiles them. It replays the pipelines earlier runs actually drew with, which
rem run-drae.cmd records by setting VICTORY_PIPELINE_BOOK=1.
rem
rem So: play once first, then run this with the game closed. A few minutes, most of the CPU.
rem Safe to stop and re-run; it continues where it left off.
rem Re-run after a graphics driver update or a change to the shader translator.
setlocal EnableExtensions
set "VICTORY_LOG=warn"
rem Warms the tree this file is in; its victory.toml names the game and cache folders.
rem DRAE_TREE=<folder> warms another tree's build (e.g. set DRAE_TREE=%~dp0..\victory-leonida).
if not defined DRAE_TREE set "DRAE_TREE=%~dp0"
cd /d "%DRAE_TREE%"
if not exist "target\release\victory.exe" (
  echo Build it first: cargo build --release --locked -p victory
  exit /b 2
)
target\release\victory.exe warm --used
