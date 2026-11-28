@echo off
rem cmd reads a batch file by byte offset WHILE it runs, so editing this file during a run made
rem the waiting cmd resume mid-line after the game closed, run garbage and RELAUNCH the game
rem with the old settings into the same log (27/11, runs 131910-2 and 133355). So it runs from a
rem copy in %%TEMP%% and this file can be edited at any time. The call, the delete and the exit
rem are on one line because cmd parses a whole line before running it. Keep this file CRLF:
rem with LF endings cmd's goto label search lost its place once the file grew (lines ran
rem with their first characters cut off, 27/11).
setlocal
if defined DRAE_COPY goto copied
set "DRAE_HERE=%~dp0"
set "DRAE_COPY=%TEMP%\run-drae-%RANDOM%%RANDOM%.cmd"
copy /y "%~f0" "%DRAE_COPY%" >nul
call "%DRAE_COPY%" %* & del "%DRAE_COPY%" & exit /b
:copied
rem Launches the victory-drae build (forked long ago from github.com/Corgansm/victory-corgi), an NVIDIA/high-memory
rem fork. These settings are ITS OWN, taken from scripts\play-highram.ps1 — the earlier copy of
rem this file used victory-fork's set, which is a different fork and does not apply here.
rem
rem   run-drae.cmd            Series X. MEASURED faster and smoother here than Series S.
rem   run-drae.cmd series-s   Series S (720p inside). Measured 3x SLOWER here - see below.
rem   run-drae.cmd safe       without the newest, faster ways — try this first if something breaks
rem   run-drae.cmd rt         the game's own ray tracing (indirect light, sun/lamp shadows), FOR REAL:
rem                            builder on, VICTORY_FIRSTLANE_OWN=1 so its last 3 programs
rem                            translate, VICTORY_BVH_MISS left unset, builder stores kept out
rem                            of title memory (QUARANTINE). Before 27/11 "rt" did nothing: the
rem                            builder never started (3 of 53 programs) and BVH_MISS=1 made
rem                            every ray miss. Needs "warm-drae.cmd rt" (own cache key).
rem                            upstream corgi README: the builder reset NVIDIA cards within seconds.
rem   run-drae.cmd pairs-all  VICTORY_PAIRED_ONLY unset: every wave64 program may take the paired form.
rem                            Needs "warm-drae.cmd pairs-all" (own cache key). See its comment below.
rem   run-drae.cmd wide       VICTORY_INDIRECT_WIDE=force: indirect dispatches run more than 65,535
rem                            groups in x. FIX CANDIDATE for the stale band at the bottom of the picture
rem                            (27/11, diag frame 2697): the title's tile lists 2dc364df5d3278b8 and
rem                            940c2635dc1312f4 ask for up to ~67,000 groups (one per 8x4 tile); cut to
rem                            65,535 the lowest tiles are never drawn (indirect.rs widest()). On by
rem                            default on AMD only; untested on NVIDIA (could lose the device).
rem   run-drae.cmd diag       the diagnostics build with the in-game debugger (see near the end)
rem   run-drae.cmd novol      VICTORY_SKIP_PIPELINES for the water's transmission volumetrics
rem                            (5dc3b25986622949 _Tiled, 6f225b641c5b0708 _Tiled_LocalLights,
rem                            a10179ab7eda0dc3 _VolumePortalsOnly): A/B for the black tiles on the water
rem                            under pairs-all. Without pairs-all the driver refuses these three anyway;
rem                            emit\mod.rs says their paired form draws dark slabs on the water. CONFIRMED
rem                            (27/11 diag A/B): black tiles gone. Works in release too since 27/11 (the
rem                            switch was diag-only). No re-warm: the cache key is unchanged.
rem   run-drae.cmd trace      (with diag) VICTORY_SHADER_TRACE for the water's black tiles (27/11, diag
rem                            frame 3058): the 1x1x1 compute e30970f1727063fd that fills the water mesh
rem                            draws' buffer (thread 0,0,0, traced on any F9) and the water pixel shaders
rem                            4cde906781d9b043 / c5d4ff74b695d1f2 (F9 over CLEAN water). Mesh shaders
rem                            cannot be traced (lower.rs skips them). Set VICTORY_SHADER_TRACE before
rem                            calling to trace other hashes (cmd splits arguments at commas).
rem   run-drae.cmd batch1     VICTORY_INDIRECT_BATCH=1 instead of 2 (see its comment below)
rem   run-drae.cmd rt-open    as rt, but builder stores DO reach title memory (no quarantine)
rem   run-drae.cmd lowram     without VICTORY_CARD_ALL/FETCH_CHECK: SLOWER typical frame (97 vs
rem                            72 ms) but NO STUTTER (0%% of frames spiked, against 21%%)
rem   run-drae.cmd thin0      VICTORY_THIN=0, the value play-highram.ps1 means to set but cannot
rem   run-drae.cmd bar        put title memory windows straight in card memory over the
rem                            resizable BAR: no CPU-side copy, no send, no change list
rem   run-drae.cmd upscaler   WITH the title's own temporal upscaler (VICTORY_OWN_UPSCALER=1). Off by
rem                            default since 27/11: A/B run 133355 showed it smears old frames into
rem                            cars in tile blocks and hides the car's sun shadow; without it the
rem                            shadows return and fps went up (17-20 vs 14-16). Without it the sky
rem                            near the sun bands. "noupscaler" is still accepted (the default).
rem   run-drae.cmd bar4       as "bar" with a 4096 MB cap instead of 2048 - only worth trying if
rem                            the frame line says windows were turned down for want of room
rem                            under the cap; profile-drae.ps1 says so in one line
rem   run-drae.cmd fasthash   VICTORY_TEXTURE_FAST_HASH=1 (patch 0001, 27/11): textures of 4 MiB
rem                            and more are hashed in 1 MiB pieces by helper threads at once,
rem                            not by one thread whole. Its commit message measured ~250 MB of
rem                            texture hashed a frame in the city, 16 ms of the frame. 2 helpers
rem                            here (a quarter of 8 threads). Same change found either way; not
rem                            in the shader cache key, no warm needed.
rem   run-drae.cmd fasthash4  as "fasthash" with 4 helpers (VICTORY_TEXTURE_HASH_THREADS=4)
rem   run-drae.cmd wcnoback   VICTORY_WC_NO_BACK=1 + VICTORY_WC_NO_BACK_MIN_KB=1024 (fqxx leonida-pr
rem                            23adcf6, 27/11): copies of 1 MiB+ wholly in the title's write-combined
rem                            memory get no write-back of shader writes. Its author measured the
rem                            copy-back step 16.2 -> 5.5 ms. Without MIN_KB cutscenes went near black.
rem                            In victory-drae master since the leonida-pr merge (9df538c).
rem   run-drae.cmd fps60      VICTORY_FRAME_INTERVAL_MS=16.667 (VIctory-NVIDIA bacb402, 27/11): our
rem                            frame clock ticks at 60 a second and the title's own 30 fps setting
rem                            is patched to 60 at load (its frame time, interval and limiter
rem                            follow). The log says "frame rate: ... 2 of 2 loads ... patched".
rem                            Only lifts the 30 cap (menus, videos, light scenes); no re-warm.
rem                            Only in builds with that port (port/nvidia-batch1 and later).
rem   run-drae.cmd dlss       DLSS Super Resolution in place of the title's upscaling (VICTORY_DLSS=1,
rem                            Quality; port/nvidia-streamline builds). The title is patched onto its
rem                            FidelityFX path (renderer 7) and DLSS takes its upscale dispatches;
rem                            VICTORY_OWN_UPSCALER is dropped for it. At the 4K output Quality draws
rem                            the scene at 2560x1440, the size the own upscaler draws, so it A/Bs
rem                            against a plain run. NVIDIA's files: Emu\nvidia-sdk (get-nvidia-sdks.ps1).
rem   run-drae.cmd framegen   DLSS Frame Generation on our present, 2x (VICTORY_FRAMEGEN=1); use it
rem                            with "dlss" (alone it leaves the upscaling to the title's FidelityFX).
rem                            "fg4" asks for 4x (RTX 50; VICTORY_FRAMEGEN_X=4).
rem   run-drae.cmd 1440       a 2560x1440 output instead of the Series X's 4K (VICTORY_NATIVE_1440P=1);
rem                            with "dlss" the scene is then 1707x960 at Quality.
rem   run-drae.cmd best       the best measured set: bar4 thin4 panel pairs-all wide novol wcnoback
rem                            (other words may follow it, e.g. "best fps60").
rem                            Best measured full command (2026-11-28): best fps60 dlss fg4 1440.
rem                            AI AGENTS: when a run measures a better set of words, update this
rem                            line, the preset below and README.md's "How to run it" step 5 and
rem                            "Where it stands" table together.
rem   run-drae.cmd ambwas0    VICTORY_AMBIENT_WAS=0 (leonida-pr dae373f): drops the title's leftover
rem                            probe light, the purple night-neon cast. No re-warm (host-side pass).
rem   Each tree (or worktree) runs its own build with its own copy of this file; a worktree needs
rem   its own victory.toml (copy the main tree's).
rem   run-drae.cmd slots-auto leave VICTORY_DYNAMIC_SLOTS unset so the ported capped-heap
rem                            default (606,784 slots) applies instead of the 524,288 set here
rem   run-drae.cmd land4      4 landing threads instead of 2 (merging fetched pieces is ~8 ms)
rem   run-drae.cmd parts      DO NOT USE: CORRUPTS GEOMETRY (26/11). Confirmed by A/B: with it,
rem                            the main character and vehicles tear into smeared triangles; drop
rem                            the word and it goes (a much smaller vehicle-only defect, which
rem                            predates it, remains). The mask narrows what is MERGED back into
rem                            title memory, so a changed 4 KiB it fails to flag is a lost shader
rem                            write. Worse at night, where more flights are in the air.
rem                            What it was for: the compare on the card hashes each 4 KiB of a
rem                            fetch brings down only the parts that changed. Run bar-142856
rem                            fetched 55.2 MB a frame of which 29.3 MB held a new byte, so this
rem                            is worth about half the fetch and half the merge. The frame line
rem                            says what it saved ("of the pieces fetched N MB lay in them whole
rem                            and M MB came down").
rem   run-drae.cmd thin4      4 list-translation threads instead of 8, on an 8-thread box
rem   run-drae.cmd frames2    let the title run TWO frames ahead of our submitting thread
rem                            (VICTORY_SUBMIT_FRAMES=2; 1 by default). This is this fork's own form
rem                            of victory-fork's VICTORY_PRESENT_QUEUED: queue/behind.rs already
rem                            puts the present in the lane and returns, and this is the bound it
rem                            waits at. Run 184604 had the title's threads standing 19.8 ms of a
rem                            50.8 ms frame at that bound while our thread worked 45 ms, so the
rem                            most this can win is the frame coming down to about 45 ms (+11%).
rem                            Costs latency: the title is two frames ahead of the picture.
rem   run-drae.cmd frames3    three frames ahead - more latency again, for the shape of the curve
rem   run-drae.cmd panel      the numbers on the picture from the first frame (VICTORY_PANEL=1).
rem                            F3 shows and hides it at any time with the window in front, so the
rem                            switch only decides whether it starts shown. Eleven lines in the
rem                            top left: frame rate and the longest frame, draws and dropped
rem                            draws, the submitting thread and its waits, the card, the list
rem                            calls translated, the biggest parts of the preparing, copy-back,
rem                            the megabytes moved, and RAM and card memory. It says what it
rem                            costs itself on the last line (tens of microseconds a frame).
rem
rem Every run writes logs\<arguments>-<time>.err.log and names it in logs\latest.txt.
rem Summarise the newest with:  powershell -ExecutionPolicy Bypass -File profile-drae.ps1
rem
rem Build first:  cargo build --release --locked -p victory     (in victory-drae)
rem Then warm:    warm-drae.cmd
rem Note: upstream corgi's own measurements are of its `diag` build; this uses `release`, which is faster
rem to play on. Its shader cache is separate from the diag build's.
setlocal EnableExtensions

rem This file's own directory, kept in a variable. After the "cd" below, cmd resolved the
rem built-in form against the CURRENT directory, not this file's (measured: the last line
rem then named victory-drae\profile-drae.ps1), so every later path uses this variable.
if defined DRAE_HERE (set "HERE=%DRAE_HERE%") else set "HERE=%~dp0"

rem --- paths. The game and cache folders come from victory.toml in this tree (git-ignored; copy
rem victory.example.toml). The exe reads it from the current folder, so this runs the tree it is in.
cd /d "%HERE%"
if not defined VICTORY_GAME_DIR if exist victory.toml for /f "usebackq tokens=1,* delims==" %%a in (`findstr /r /b /c:"game_dir *=" victory.toml`) do set "VICTORY_GAME_DIR=%%b"
if defined VICTORY_GAME_DIR set "VICTORY_GAME_DIR=%VICTORY_GAME_DIR:'=%"
if defined VICTORY_GAME_DIR set "VICTORY_GAME_DIR=%VICTORY_GAME_DIR:"=%"
if defined VICTORY_GAME_DIR for /f "tokens=* delims= " %%g in ("%VICTORY_GAME_DIR%") do set "VICTORY_GAME_DIR=%%g"
if not defined VICTORY_GAME_DIR (
  echo No game folder: copy victory.example.toml to victory.toml here and set game_dir, or set VICTORY_GAME_DIR. 1>&2
  exit /b 2
)
if not defined VICTORY_LOG set "VICTORY_LOG=info"
rem Records every pipeline the title creates, so warm-drae-pipelines.cmd can pre-build them for
rem the driver afterwards. Not part of the shader cache key.
set "VICTORY_PIPELINE_BOOK=1"

rem --- the base set (scripts\play.ps1)
set "VICTORY_TEXTURE_NO_WAIT=1"
set "VICTORY_RELAY=2"
set "VICTORY_WIDE_BACK=1"
set "VICTORY_CARD_REHASH_RUNS=1"
set "VICTORY_JOIN_COPIES=1"
set "VICTORY_INDIRECT_WINDOW=1"
set "VICTORY_RELAY_COPY=1"
set "VICTORY_CARD_STEPS=2"
rem BATCH=2 lets the title's tile-class dispatches run without our waits between them; its own
rem comment (draw.rs ~244) says it is not known whether the title waits in ways we cannot see.
rem SUSPECT (27/11) for the screen-locked stale band at the bottom of the picture: a class
rem reading its tile count before the pass writing it ends covers only the top of the list.
rem "batch1" sets the safe default (1). Not in the shader cache key: no warm needed.
set "VICTORY_INDIRECT_BATCH=2"
set "VICTORY_MAP_THREADS=1"
set "VICTORY_TITLE_PRIORITIES=0"
set "VICTORY_CARD_AWAKE=1"
set "VICTORY_SHOW_AFTER=1"
set "VICTORY_AMBIENT=0.8"
rem Bounds what the air scatters by a multiple of the light around it. WITHOUT this, the Air
rem pass is skipped outright (ambient.rs:440) and the title's inscattering volume keeps the
rem sky's light everywhere - a grey veil over the whole picture at sunset. cool_fox's
rem launcher set this; this repo's own scripts do not.
set "VICTORY_AMBIENT_AIR=1.5"
set "VICTORY_D3D12_DRED=light"
set "VICTORY_SKIP_PIPELINES=none"

rem --- arguments. Every one is read, in any order, so they can be combined:
rem   run-drae.cmd series-x lowram     Series X profile AND without CARD_ALL
set "A_SAFE="
set "A_RT="
set "A_RTOPEN="
set "A_PAIRSALL="
set "A_BATCH1="
set "A_DIAG="
set "A_LOWRAM="
set "A_THIN0="
set "A_SERIESS="
set "A_NOUPSCALER="
set "A_UPSCALER="
set "A_BAR="
set "A_SLOTSAUTO="
:args
if "%~1"=="" goto argsdone
if /i "%~1"=="safe"     set "A_SAFE=1"
if /i "%~1"=="rt"       set "A_RT=1"
if /i "%~1"=="rt-open"  set "A_RT=1"
if /i "%~1"=="rt-open"  set "A_RTOPEN=1"
if /i "%~1"=="lowram"   set "A_LOWRAM=1"
if /i "%~1"=="thin0"    set "A_THIN0=1"
if /i "%~1"=="series-s"    set "A_SERIESS=1"
if /i "%~1"=="noupscaler"  set "A_NOUPSCALER=1"
if /i "%~1"=="upscaler"    set "A_UPSCALER=1"
if /i "%~1"=="bar"         set "A_BAR=1"
if /i "%~1"=="bar4"        set "A_BAR=1"
if /i "%~1"=="bar4"        set "VICTORY_WINDOWS_BAR_MB=4096"
if /i "%~1"=="fasthash"    set "VICTORY_TEXTURE_FAST_HASH=1"
if /i "%~1"=="fasthash4"   set "VICTORY_TEXTURE_FAST_HASH=1"
if /i "%~1"=="fasthash4"   set "VICTORY_TEXTURE_HASH_THREADS=4"
if /i "%~1"=="wcnoback"    set "VICTORY_WC_NO_BACK=1"
if /i "%~1"=="wcnoback"    set "VICTORY_WC_NO_BACK_MIN_KB=1024"
if /i "%~1"=="ambwas0"     set "VICTORY_AMBIENT_WAS=0"
if /i "%~1"=="slots-auto"  set "A_SLOTSAUTO=1"
if /i "%~1"=="land4"       set "VICTORY_LAND_THREADS=4"
if /i "%~1"=="thin4"       set "VICTORY_THIN_THREADS=4"
if /i "%~1"=="parts"       set "VICTORY_FETCH_PARTS=1"
if /i "%~1"=="frames2"     set "VICTORY_SUBMIT_FRAMES=2"
if /i "%~1"=="frames3"     set "VICTORY_SUBMIT_FRAMES=3"
if /i "%~1"=="panel"       set "VICTORY_PANEL=1"
if /i "%~1"=="pairs-all"   set "A_PAIRSALL=1"
if /i "%~1"=="batch1"      set "A_BATCH1=1"
if /i "%~1"=="diag"        set "A_DIAG=1"
if /i "%~1"=="wide"        set "VICTORY_INDIRECT_WIDE=force"
if /i "%~1"=="trace"       set "A_TRACE=1"
if /i "%~1"=="novol"       set "A_NOVOL=1"
if /i "%~1"=="fps60"       set "VICTORY_FRAME_INTERVAL_MS=16.667"
if /i "%~1"=="dlss"        set "VICTORY_DLSS=1"
if /i "%~1"=="framegen"    set "VICTORY_FRAMEGEN=1"
if /i "%~1"=="fg4"         set "VICTORY_FRAMEGEN=1"
if /i "%~1"=="fg4"         set "VICTORY_FRAMEGEN_X=4"
if /i "%~1"=="1440"        set "VICTORY_NATIVE_1440P=1"
if /i "%~1"=="best"        set "A_BAR=1"
if /i "%~1"=="best"        set "VICTORY_WINDOWS_BAR_MB=4096"
if /i "%~1"=="best"        set "VICTORY_THIN_THREADS=4"
if /i "%~1"=="best"        set "VICTORY_PANEL=1"
if /i "%~1"=="best"        set "A_PAIRSALL=1"
if /i "%~1"=="best"        set "VICTORY_INDIRECT_WIDE=force"
if /i "%~1"=="best"        set "A_NOVOL=1"
if /i "%~1"=="best"        set "VICTORY_WC_NO_BACK=1"
if /i "%~1"=="best"        set "VICTORY_WC_NO_BACK_MIN_KB=1024"
shift
goto args
:argsdone
if defined A_BATCH1 set "VICTORY_INDIRECT_BATCH=1"

rem --- the F1 menu (port/nvidia-streamline builds): what it saved, in drae-settings.txt beside
rem this file, goes over the arguments ("RESET ALL" in the menu deletes the file). An empty value
rem unsets the switch. "SAVE AND RESTART" starts this file again with the same arguments, in the
rem same tree. VICTORY_MENU_FRAMEGEN is the menu's frame generation (0, or 2..6 times).
set "VICTORY_MENU_FILE=%HERE%drae-settings.txt"
set "VICTORY_MENU_PLAY=%HERE%run-drae.cmd"
set "VICTORY_MENU_ARGS=%*"
set "VICTORY_MENU_DIR=%CD%"
if exist "%VICTORY_MENU_FILE%" for /f "usebackq eol=# tokens=1,* delims==" %%a in ("%VICTORY_MENU_FILE%") do set "%%a=%%b"
if exist "%VICTORY_MENU_FILE%" echo Settings from the F1 menu: %VICTORY_MENU_FILE%
if "%VICTORY_MENU_FRAMEGEN%"=="0" set "VICTORY_FRAMEGEN="
if defined VICTORY_MENU_FRAMEGEN if not "%VICTORY_MENU_FRAMEGEN%"=="0" set "VICTORY_FRAMEGEN=1"
if defined VICTORY_MENU_FRAMEGEN if not "%VICTORY_MENU_FRAMEGEN%"=="0" set "VICTORY_FRAMEGEN_X=%VICTORY_MENU_FRAMEGEN%"
if not defined VICTORY_MENU_FRAMEGEN if defined VICTORY_FRAMEGEN if defined VICTORY_FRAMEGEN_X set "VICTORY_MENU_FRAMEGEN=%VICTORY_FRAMEGEN_X%"
if not defined VICTORY_MENU_FRAMEGEN if defined VICTORY_FRAMEGEN set "VICTORY_MENU_FRAMEGEN=2"
if not defined VICTORY_MENU_FRAMEGEN set "VICTORY_MENU_FRAMEGEN=0"
rem DLSS without frame generation: our swap chain is made Streamline's proxy all the same, or it
rem never sees a present ("presentCommon() was not observed", 27/11) and keeps what it retired.
if defined VICTORY_DLSS if not defined VICTORY_FRAMEGEN if not defined VICTORY_STREAMLINE_PRESENT set "VICTORY_STREAMLINE_PRESENT=1"
rem Skipping big copy-backs without the 1 MiB floor turned cutscenes near black (leonida-pr).
if defined VICTORY_WC_NO_BACK if not defined VICTORY_WC_NO_BACK_MIN_KB set "VICTORY_WC_NO_BACK_MIN_KB=1024"

rem --- the faster ways, unless "safe"
if defined A_SAFE (
  set "VICTORY_UNDER_WIDE=0"
  set "VICTORY_LAND_ECHO=0"
) else (
  set "VICTORY_THIN=1"
  set "VICTORY_JOIN_PARTS=8"
  set "VICTORY_UPLOAD_AHEAD=1"
)

rem --- the high-memory NVIDIA additions (scripts\play-highram.ps1).
rem DYNAMIC_SLOTS=524288 matters on this card: NVIDIA caps descriptor heaps at 1,000,000 while the
rem title asks for 1,245,184, which otherwise leaves a large share of its slots reading null.
set "VICTORY_PIPELINE_WAIT_MS=10000"
rem "slots-auto" leaves VICTORY_DYNAMIC_SLOTS unset, which the ported capped-heap default
rem (descriptor/host.rs, capped_dynamic_slots) answers with 606,784 on this 1,000,000-view heap
rem by taking the title's unused upper ring - more than the 524,288 asked for here. The fork
rem measured no table refused at 606,784 against 13,000-60,000 per 5 s at 262,144.
if not defined A_SLOTSAUTO set "VICTORY_DYNAMIC_SLOTS=524288"
rem The title's OWN temporal upscaler, forced on every profile (Series X would not pick it).
rem SUSPECT for black street lamps and specular highlights: a temporal upscaler clamps each
rem pixel against its neighbours' history, and very bright HDR outliers are exactly what such
rem clamping turns to black. cool_fox never set this and did not show the defect.
rem Off unless "upscaler" (27/11, see the argument list). The log tag keeps "-noupscaler" so
rem log names stay comparable with runs from before the default changed.
if not defined A_UPSCALER set "A_NOUPSCALER=1"
if defined A_UPSCALER set "A_NOUPSCALER="
if not defined A_NOUPSCALER set "VICTORY_OWN_UPSCALER=1"
rem Every ray misses (no node is read) - only right while the builder is off. "rt" leaves it
rem unset; with it set a built structure is never hit. Part of the shader cache key.
if not defined A_RT set "VICTORY_BVH_MISS=1"
set "VICTORY_CARD_BUFFERS_MIN_MB=1"
set "VICTORY_CARD_BUFFERS_MB=1536"
set "VICTORY_COMPOSE=1"

rem The nearest thing a PC has to the console's unified memory. Title memory windows go
rem straight into card memory that the CPU maps (D3D12_HEAP_TYPE_GPU_UPLOAD): no copy on the
rem CPU's side, nothing to send, and no list of changes to keep. Needs a resizable BAR -
rem this card reports BAR1 = 16384 MiB, the whole of its memory, so it has one.
rem Windows only go there while a fifth of the card's budget is free.
rem Set VICTORY_WINDOWS_BAR_MB in the window before calling this to ask for another size; the
rem frame line "windows outside buffers, by where their bytes lie" says whether a larger one
rem would take any more windows ("for want of room under the cap") or whether the card is full.
if defined A_BAR if not defined VICTORY_WINDOWS_BAR_MB set "VICTORY_WINDOWS_BAR_MB=2048"

rem --- settings play-highram.ps1 sets that the earlier copy of this file missed.
rem NOTE: play-highram.ps1 itself does not run. It sets VICTORY_THIN twice in one hashtable
rem (lines 27 and 46), which is a PowerShell parse error, so nobody has run this fork through it.

rem 16-bit float results rounded to nearest: this fork's fix for banding on NVIDIA.
set "VICTORY_F16_NEAREST=1"

rem The paired wave64 form, limited to the two programs it draws CORRECTLY (the alpha
rem transmission prefilter and particle visibility). Left unset, every program NVIDIA
rem refuses takes that form - including the water's transmission volumetrics, which in
rem it draw DARK SLABS ON THE WATER (play-highram.ps1:42-45).
rem 27/11: with this list, 9 OTHER wave64 compute programs (e.g. 407eca669212d400, 6b82bc7d22016cb5)
rem get no paired form, stay wave64 and the driver refuses their pipelines (E_INVALIDARG, in every
rem run since the list was added; run 132221 without it made them all). Their dispatches are dropped,
rem 2 indirect a frame among them. Suspect for the screen-locked stale tiles at the bottom of the
rem picture. "pairs-all" leaves the list unset (the water slabs may come back).
if not defined A_PAIRSALL set "VICTORY_PAIRED_ONLY=b5b838548eaa4050638308fb2c805641,2431bbe7dc154ad759d20bc954829c1c"

rem The -HighRam pair, on by default in play-highram.ps1: every title buffer copied to the
rem card, and relay results checked. upstream corgi's README credits these with taking the per-frame
rem wait for the card from 58 ms to 0. They cost card memory, and this box already sat at
rem 11.4 of 16.3 GB without them - so "lowram" leaves them out. MEASURED 26/11: they take the
rem median frame from 97 ms to 72 ms AND the wait for the card to 0, but about a fifth of frames
rem then spike (worst seen 10 s). That is the stuttering. Use "lowram" if smoothness matters more.
if not defined A_LOWRAM (
  set "VICTORY_CARD_ALL=1"
  set "VICTORY_FETCH_CHECK=1"
)

rem play-highram.ps1 MEANS to set VICTORY_THIN=0: its comment says that with THIN=1 on
rem NVIDIA's capped descriptor pool about 500 tables a frame were refused and about 300
rem indirect draws dropped, the water went missing in tiles, and the picture melted the
rem longer you played. THIN=0 costs about 1.5 fps. Pass "thin0" to try it.
if defined A_THIN0 set "VICTORY_THIN=0"

rem --- the game's real ray-traced indirect light, on request
if defined A_RT set "VICTORY_BVH_BUILDER=1"
rem The builder's scans read a per-wave sum with v_readfirstlane in a loop; without this
rem three of its 53 programs do not translate and the builder stays off (lower.rs ~9485).
rem Part of the shader cache key.
if defined A_RT set "VICTORY_FIRSTLANE_OWN=1"
if defined A_RT if not defined A_RTOPEN set "VICTORY_BVH_QUARANTINE=1"

rem --- the log of the run. Both streams go to Emu\logs, named after the arguments and the
rem time, and logs\latest.txt names the newest, which profile-drae.ps1 reads when given no
rem argument. The console window shows nothing while the title runs; the log has all of it.
set "TAG=drae"
rem A worktree (its .git is a file, not a folder, e.g. victory-leonida): its folder name goes in the log name.
if not exist "%HERE%.git\" for %%D in ("%HERE%.") do set "TAG=%%~nxD"
if defined A_BAR set "TAG=%TAG%-bar"
if defined A_SERIESS set "TAG=%TAG%-seriess"
rem "rton", not "rt": logs named -rt- before 27/11 are runs where ray tracing never ran.
if defined A_RT if not defined A_RTOPEN set "TAG=%TAG%-rton"
if defined A_RTOPEN set "TAG=%TAG%-rtopen"
if defined A_LOWRAM set "TAG=%TAG%-lowram"
if defined A_THIN0 set "TAG=%TAG%-thin0"
if defined A_SAFE set "TAG=%TAG%-safe"
if defined A_NOUPSCALER set "TAG=%TAG%-noupscaler"
if defined A_SLOTSAUTO set "TAG=%TAG%-slotsauto"
if defined VICTORY_LAND_THREADS set "TAG=%TAG%-land%VICTORY_LAND_THREADS%"
if defined VICTORY_THIN_THREADS set "TAG=%TAG%-thin%VICTORY_THIN_THREADS%"
if defined VICTORY_WINDOWS_BAR_MB set "TAG=%TAG%-%VICTORY_WINDOWS_BAR_MB%mb"
if defined VICTORY_FETCH_PARTS set "TAG=%TAG%-parts"
if defined VICTORY_TEXTURE_FAST_HASH set "TAG=%TAG%-fasthash%VICTORY_TEXTURE_HASH_THREADS%"
if defined VICTORY_SUBMIT_FRAMES set "TAG=%TAG%-frames%VICTORY_SUBMIT_FRAMES%"
if defined VICTORY_WC_NO_BACK set "TAG=%TAG%-wcnoback"
if defined VICTORY_AMBIENT_WAS set "TAG=%TAG%-ambwas%VICTORY_AMBIENT_WAS%"
if defined VICTORY_PANEL set "TAG=%TAG%-panel"
if defined A_PAIRSALL set "TAG=%TAG%-pairsall"
if defined A_BATCH1 set "TAG=%TAG%-batch1"
if defined A_DIAG set "TAG=%TAG%-diag"
if defined A_TRACE if not defined VICTORY_SHADER_TRACE set "VICTORY_SHADER_TRACE=e30970f1727063fd,4cde906781d9b043,c5d4ff74b695d1f2"
if defined A_TRACE set "TAG=%TAG%-trace"
if defined A_NOVOL set "VICTORY_SKIP_PIPELINES=5dc3b25986622949,6f225b641c5b0708,a10179ab7eda0dc3"
if defined A_NOVOL set "TAG=%TAG%-novol"
if defined VICTORY_INDIRECT_WIDE set "TAG=%TAG%-wide"
if defined VICTORY_FRAME_INTERVAL_MS set "TAG=%TAG%-fps60"
if defined VICTORY_DLSS set "TAG=%TAG%-dlss"
if defined VICTORY_FRAMEGEN set "TAG=%TAG%-fg%VICTORY_FRAMEGEN_X%"
if defined VICTORY_NATIVE_1440P set "TAG=%TAG%-1440"
rem NVIDIA's Streamline files (DLSS, frame generation), fetched by get-nvidia-sdks.ps1.
if not defined VICTORY_STREAMLINE_DIR set "VICTORY_STREAMLINE_DIR=%HERE%nvidia-sdk\Streamline\bin\x64"
if not exist "%VICTORY_STREAMLINE_DIR%" if exist "%HERE%..\nvidia-sdk\Streamline\bin\x64" set "VICTORY_STREAMLINE_DIR=%HERE%..\nvidia-sdk\Streamline\bin\x64"
if defined VICTORY_QUICK_FIND_SLOTS set "TAG=%TAG%-slots%VICTORY_QUICK_FIND_SLOTS%"
for /f "tokens=1-3 delims=:., " %%a in ("%TIME%") do set "STAMP=%%a%%b%%c"
set "STAMP=%STAMP: =0%"
if not exist "%HERE%logs" mkdir "%HERE%logs"
rem Never overwrite a log. A run on 26/11 reused the name of an earlier one and truncated
rem 500 s of measurements to 15; the stamp alone is not proof of a free name, so the name is
rem tried with -2, -3 ... until nothing is there.
rem (No parentheses: inside a block cmd expands %LOGN% when it parses the block, so the
rem counter would never move. Line by line with a goto, each expansion is fresh.)
set "LOG=%HERE%logs\%TAG%-%STAMP%"
set /a LOGN=1
:logname
if not exist "%LOG%.err.log" goto lognamefree
set /a LOGN+=1
set "LOG=%HERE%logs\%TAG%-%STAMP%-%LOGN%"
goto logname
:lognamefree
> "%HERE%logs\latest.txt" echo %LOG%.err.log

rem "diag": the diagnostics build (own shader cache: warm-drae.cmd diag ...) with the in-game
rem debugger on, its folder Emu\debugger\<log name>. F9 records the next frame and the pixel
rem under the mouse (pixel history: every draw/dispatch that changed it); F10 overlay; F11 as F9
rem with texture pictures. Or write "capture pick <x> <y>" (fractions) into command.txt there.
set "EXE=target\release\victory.exe"
if defined A_DIAG set "EXE=target\diag\victory.exe"
if defined A_DIAG for %%L in ("%LOG%") do set "VICTORY_DEBUGGER=%HERE%debugger\%%~nxL"
if not exist "%EXE%" (
  echo Build it first: cargo build --release --locked -p victory  ^(diag: --profile diag --features diagnostics^) 1>&2
  exit /b 2
)
rem --- the console profile. play-highram.ps1 uses Series S unless -SeriesX, but MEASURED HERE on
rem 26/11 Series S was far worse than Series X on the same build and settings, so Series X is the
rem default and "series-s" asks for the other one:
rem   Series X: median 97 ms a frame, 0 of 17 gameplay frames spiked, 375-477 draws dropped
rem   Series S: median 295 ms a frame, 5 of 24 spiked (worst 2654 ms), 2094-2438 draws dropped
rem Lower internal resolution did NOT make it faster: it dropped five times as many draws.
set "CONSOLE="
if defined A_SERIESS set "CONSOLE=--console series-s"
echo Starting victory-drae %CONSOLE%...
echo Logging to %LOG%.err.log
%EXE% run "%VICTORY_GAME_DIR%\game_scarlett_master.exe" %CONSOLE% --execute > "%LOG%.log" 2> "%LOG%.err.log"
echo.
echo Summarise the run with:
echo    powershell -ExecutionPolicy Bypass -File "%HERE%profile-drae.ps1"
