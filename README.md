# victory-drae

VIctory for NVIDIA RTX cards on PCs with plenty of memory: DLSS, DLSS frame generation, the
title's 60 fps frame interval and an F1 settings menu, with a launcher that keeps every
measured setting as a word you can pass to it.

**Where it comes from:** [Marvisak/VIctory](https://github.com/Marvisak/VIctory), the
compatibility layer that runs an Xbox Series title natively on Windows from files you supply →
[Corgansm/victory-corgi](https://github.com/Corgansm/victory-corgi), the NVIDIA/high-memory fork
this one was forked from → **victory-drae**. It has diverged a long way from corgi since then.
Corgi's own notes are kept further down ([Inherited from victory-corgi](#inherited-from-victory-corgi-2026-11-25)).
It also takes changes from [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA)
(Streamline DLSS and frame generation, the F1 menu), fxceless/victory-fork and others; every
commit taken from elsewhere says where it came from.

**This repository contains no game files, code or assets.** You need your own copy of the game.
It is not affiliated with or endorsed by the game's developer or publisher, Microsoft or NVIDIA.
See [docs/asset-policy.md](docs/asset-policy.md).

## Where it stands (2026-11-28)

Measured on one PC: Ryzen 7 9800X3D (SMT off, 8 threads), GeForce RTX 5080 16 GB, 64 GB RAM,
Windows 11, Series X profile.

| Launcher words | Result |
|---|---|
| `best fps60 dlss fg4 1440` (recommended) | about **31 fps rendered**, 2.8-4x shown with frame generation, 2560x1440 output |
| `best fps60 dlss` | about 21-22 fps, 4K output, no frame generation |
| `best` | about 20 fps median |

## How to run it

You need Windows 10/11 x64, an NVIDIA RTX card (RTX 40 or 50 for frame generation), 32 GB of RAM
or more (64 GB is what this was tuned on), an SSD, Rust (the toolchain is pinned in
`rust-toolchain.toml`), the MSVC build tools and your own dump of the title (Xbox Series build
1.0.2.1).

1. **Point it at the game.** Copy `victory.example.toml` to `victory.toml` (git-ignored) and set
   `game_dir` to your game folder, the one with `game_scarlett_master.exe`. Set `cache_dir` too if
   you want the cache somewhere other than `%LOCALAPPDATA%\victory\cache`. Both must be outside
   this folder.

   ```toml
   [paths]
   game_dir = 'D:\games\your-dump'
   cache_dir = 'D:\victory-data\cache'
   ```

2. **Build it.**

   ```
   cargo build --release --locked -p victory
   ```

3. **Fetch NVIDIA's DLSS files** (about 70 MB, from NVIDIA's own GitHub, into `nvidia-sdk\`). Run
   this once. Without the files the game still runs, with its own upscaling and no frame generation.

   ```
   Get-NVIDIA-DLSS.cmd
   ```

4. **Warm the shader cache.** This translates every shader ahead of time, so the first run does
   not stutter. `best` uses `pairs-all`, which has a cache key of its own, so warm with that word:

   ```
   warm-drae.cmd pairs-all
   ```

   Run it again after pulling changes to the shader translator. Entries are keyed on its source,
   so stale ones are never used.

5. **Run it.**

   <!--
   NOTE FOR AI AGENTS: the command below is the best measured launcher configuration. When a run
   measures a new best set of run-drae.cmd words (higher fps with a clean picture, compared one
   variable at a time), update this command, the "Where it stands" table above (with the date and
   the measured fps) and the "best" preset's comment in run-drae.cmd in the same change.
   -->

   ```
   run-drae.cmd best fps60 dlss fg4 1440
   ```

   - `best` is the best measured set of settings: `bar4 thin4 panel pairs-all wide novol wcnoback`.
   - `fps60` lifts the title's own 30 fps frame interval.
   - `dlss` lets DLSS do the upscaling, `fg4` turns on DLSS frame generation at up to 4 frames
     shown per frame rendered, and `1440` gives a 2560x1440 output instead of 4K.

   Every other word the launcher takes is listed, with what it was measured to do, at the top of
   [run-drae.cmd](run-drae.cmd). F1 opens the settings menu and F3 hides the frame-rate panel.
   Close the game before starting a new run.

6. **Warm the driver's pipelines (once, after the first run).** Every run records the pipelines
   the title asks for. With the game closed, this builds them for the driver (a few minutes, most
   of the CPU), so later runs neither stutter nor draw black while the driver compiles:

   ```
   warm-drae-pipelines.cmd
   ```

   Run it again after a graphics driver update or a re-warm in step 4. If a run sits at a flat
   14 fps, the driver's cache is cold: run this, then start again.

Each run logs to `logs\<words>-<time>.err.log`, and `logs\latest.txt` names the newest. To
summarise the last run on one page:

```
powershell -ExecutionPolicy Bypass -File profile-drae.ps1
```

Run it from a normal window, not as administrator. A firewall rule that blocks
`target\release\victory.exe` from the network is a good idea: the title tries to send crash
reports.

## Inherited from victory-corgi (2026-11-25)

The rest of this page is corgi's README as it was when this fork started, kept for its notes and
credits. Its figures are from corgi's own test machine (RTX 5090 Laptop, Series S profile), not
from this fork. Its `scripts\play-highram.ps1` is still in the tree but is not how this fork is run.

### Where it stands (2026-11-25)

Tested on one machine only: Windows 11, Intel Core Ultra 9 275HX, NVIDIA GeForce RTX 5090 Laptop
GPU (24 GB), 64 GB RAM, Series S profile.

| | |
|---|---|
| Menus, videos, loading screens | 30 fps |
| Opening cutscenes | 40-42 ms a frame (**24-25 fps**) |
| First mission, on the boat | 60-75 ms a frame (**13-16 fps**) with the launcher's settings |
| Longest session so far | 30 minutes and still running, no crash and no device loss |
| Waiting for the graphics card each frame | 0 ms (it was 58 ms before `VICTORY_CARD_ALL`) |
| Mouths move in cutscenes | yes |
| Boats float | yes |

The same build with the parent forks' settings ran at 9-13 fps on this PC; the jump came from one
switch, `VICTORY_CARD_ALL=1` (below).

New on 2026-11-26: **no blue blocks in the sky and no black slabs on the water.** NVIDIA's
driver refused eight of the title's compute programs ("Shader wants wave lane count of 64":
its waves have 32 lanes, the console's 64). fxceless pull request 854 (PiloteCode) translates
such programs in a paired form, each console wave in one host wave with two lanes a thread; it
is ported here, and the alpha transmission prefilter in that form removes the blue blocks in
the sky. The same form for the water's transmission volumetrics drew black slabs under
islands, piers and bridges, so those stay out (`VICTORY_PAIRED_ONLY` in the launcher); and the
PR also moved 60 programs that already worked (the temporal resolves among them) to that
form, which here melted the picture: they keep their earlier translation. Float compares now
tell NaN by its bits (`VICTORY_NAN_EXACT`), as NVIDIA's driver may fold a compare's NaN case
away. The frame rate shows on the picture (`VICTORY_PANEL=1`, F3 hides it).

Also new: **no more melting.** With the title's list calls recorded on threads of their own
(`VICTORY_THIN=1`, the base fork's default), NVIDIA's capped descriptor pool ran full: about
500 descriptor tables a frame refused and 300 indirect draws dropped, so the water went
missing in tiles and the picture smeared and melted the longer one played (the same as
avatarmanz/VIctory-7900XTX issues 35, 62 and 112 on other NVIDIA cards). The launcher now sets
`VICTORY_THIN=0`: 11 refusals a frame on average, a clean picture, about 1.5 fps slower.

What is known to be wrong:

- Now and then a texture flickers or the frame stutters when a new area loads: the descriptor
  pool still runs full for a moment there.
- Lighting is flat and colours are off indoors and in shade: the title's ray-traced light does
  not run yet (`VICTORY_BVH_BUILDER=1` resets the card within seconds on NVIDIA).
- The water is the darker blue of the parent forks: its transmission volumetrics are off
  until their paired form draws right.
- A faint grid of darker diamonds on the near water in some views.
- In-world screens (the boat's navigation display) stay dark.
- The picture is softer than the Series X profile: the world is rendered at the Series S size and
  upscaled by the title's own upscaler.
- The first run with a new build or driver stutters while pipelines are made.

### Why it needs a lot of memory

`VICTORY_CARD_ALL=1` gives every title buffer a copy in the graphics card's memory. Without it,
buffers that live only in the PC's memory make the thread that prepares each frame wait for the
card to finish before it may read them: 58 ms of a 104 ms frame on this PC. With it the wait is
gone, at the cost of video memory (this PC has 24 GB). The parent fork measured 40-46 GB of
private memory: **64 GB of RAM and a 16-24 GB card are what this was tuned on.** On smaller cards,
leave `-HighRam` off in the launcher (below) and compare.

### What is in it

This is a tree of other people's work, combined and tested on one NVIDIA laptop. The history is
kept: every commit names its author, and commits taken from elsewhere say where from.

| From | What |
|---|---|
| [avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX) (the base) | Threads for the title's list calls (`VICTORY_THIN`), submissions prepared on a thread of their own, write relay, card fills, textures on a copy queue and shown only after their upload ran (`VICTORY_SHOW_AFTER`), texture read-back for the water (boats float). Its own credits: fxceless/victory-fork, DexNom, Belobug, ZJOU-VICTORY, myokex, Geckomacabre and more ([docs/README-avatarmanz-cobrez.md](docs/README-avatarmanz-cobrez.md)). |
| [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA) | NVIDIA's view heap is capped at 1,000,000: more dynamic descriptor slots (`VICTORY_DYNAMIC_SLOTS`), which ended thousands of dropped draws a frame; the Series S interface plane (`VICTORY_COMPOSE`, after fxceless pull request 1189 by Tissma95); render scale, the title's own upscaler and the watchdog patches (after ZigZagBoi1/ScarlettPort); ray walks ended at the root (`VICTORY_BVH_MISS`). |
| [cobrez/Victory-Nvidia-4090-laptop](https://github.com/cobrez/Victory-Nvidia-4090-laptop) | The high-performance adapter on laptops with two GPUs, idle lists released when memory is low, and the `-Exact` pair `VICTORY_FETCH_CHECK` / `VICTORY_CARD_ALL` (relay results and title copies reach the right submission), which is what made this PC fast. |
| fxceless/victory-fork pull request 854 (PiloteCode) | Wave64 programs in a paired form on 32-lane hosts, mesh programs over 32 KiB linked with their pixel program. Here only for programs no other form takes, and only those listed in `VICTORY_PAIRED_ONLY`. |
| fxceless/victory-fork pull request 927 (tippythetaidum) | Halves moved through 16-bit buffer views keep their bits: the network that moves the faces works, so mouths move. Combined here with the base's own form for views picked from the heap. |
| Belobug/VIctory-Belobug-AMD pull request 181 | The BCPack BC7 dictionary's first entry read whole: no more coloured 4x4 blocks on clothes and faces. |
| This fork | `VICTORY_F16_NEAREST` in integer arithmetic (16-bit float results rounded to nearest on NVIDIA, which rounds toward zero: no banding), quad reads in pixel programs (no lines along triangle edges), a guard for a script crash in the first boat mission (`VICTORY_SCRIPT_NULL_COPY`), NaN-exact float compares (`VICTORY_NAN_EXACT`), the paired form's group sync for one-wave groups (`VICTORY_PAIRED_SYNC`) and per-program choice (`VICTORY_PAIRED_ONLY`), the merge, the measurements and `scripts\play-highram.ps1`. |


## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not covered is named in
[docs/asset-policy.md](docs/asset-policy.md).
