@echo off
rem Translates every shader into victory-corgi's own shader cache ahead of a run, so the game does
rem not translate them while you play. A couple of minutes, CPU only.
rem
rem Uses corgi's `shaders` command, which opens the GPU itself, so wave size and descriptor limits
rem match a real run; the earlier copy of this file used victory-fork's `translate --warm` and read
rem its switches out of run-victory.cmd, which is a different fork.
rem
rem The settings below are copied from run-corgi.cmd and must be kept in step with it: some are part
rem of the shader cache key, and entries warmed under different ones are never found at run time.
rem Re-run after updating corgi (the cache is keyed on the translator's source). The diag build has a
rem SEPARATE cache: warm that with target\diag\victory.exe shaders.
setlocal EnableExtensions
set "VICTORY_GAME_DIR=C:\Users\drae\Desktop\GTAVI\GTA6.Xbox.v1.0.2.1-CYBERLEEK"
set "VICTORY_CACHE_DIR=C:\Users\drae\AppData\Local\VIctory\cache-corgi"
set "VICTORY_TEXTURE_NO_WAIT=1"
set "VICTORY_RELAY=2"
set "VICTORY_WIDE_BACK=1"
set "VICTORY_CARD_REHASH_RUNS=1"
set "VICTORY_JOIN_COPIES=1"
set "VICTORY_INDIRECT_WINDOW=1"
set "VICTORY_RELAY_COPY=1"
set "VICTORY_CARD_STEPS=2"
set "VICTORY_INDIRECT_BATCH=2"
set "VICTORY_MAP_THREADS=1"
set "VICTORY_TITLE_PRIORITIES=0"
set "VICTORY_CARD_AWAKE=1"
set "VICTORY_SHOW_AFTER=1"
set "VICTORY_AMBIENT=0.8"
set "VICTORY_D3D12_DRED=light"
set "VICTORY_SKIP_PIPELINES=none"
set "VICTORY_THIN=1"
set "VICTORY_JOIN_PARTS=8"
set "VICTORY_UPLOAD_AHEAD=1"
set "VICTORY_PIPELINE_WAIT_MS=10000"
set "VICTORY_DYNAMIC_SLOTS=524288"
set "VICTORY_OWN_UPSCALER=1"
rem "warm-corgi.cmd rt" warms for "run-corgi.cmd rt" / "rt-open": those two switches are in the
rem shader cache key, so the rt runs need entries of their own.
rem Arguments, any order: rt, pairs-all, diag (as in run-corgi.cmd).
set "W_RT="
set "W_PAIRSALL="
set "W_DIAG="
:wargs
if "%~1"=="" goto wargsdone
if /i "%~1"=="rt" set "W_RT=1"
if /i "%~1"=="pairs-all" set "W_PAIRSALL=1"
if /i "%~1"=="diag" set "W_DIAG=1"
shift
goto wargs
:wargsdone
if defined W_RT (set "VICTORY_FIRSTLANE_OWN=1") else set "VICTORY_BVH_MISS=1"
set "VICTORY_CARD_BUFFERS_MIN_MB=1"
set "VICTORY_CARD_BUFFERS_MB=1536"
set "VICTORY_COMPOSE=1"
rem Both of these are part of the SHADER CACHE KEY (shader_cache.rs:65-83): entries warmed
rem without them are never found by a run that sets them. Keep in step with run-corgi.cmd.
set "VICTORY_F16_NEAREST=1"
if not defined W_PAIRSALL set "VICTORY_PAIRED_ONLY=b5b838548eaa4050638308fb2c805641,2431bbe7dc154ad759d20bc954829c1c"
set "VICTORY_LOG=warn"
set "EXE=target\release\victory.exe"
if defined W_DIAG set "EXE=target\diag\victory.exe"
if not exist "%EXE%" (
  echo Build it first: cargo build --release --locked -p victory  ^(diag: --profile diag --features diagnostics^)
  exit /b 2
)
%EXE% shaders --jobs %NUMBER_OF_PROCESSORS%
