<#
.SYNOPSIS
  Summarises a victory-drae run log into one page of numbers.

.DESCRIPTION
  run-drae.cmd writes logs\<arguments>-<time>.err.log and names the newest in
  logs\latest.txt. This reads one of those, takes every frame-rate line and the
  per-frame lines beside it, and prints medians rather than the last line, so a
  single spike cannot speak for the run.

  Each frame-rate line covers about a second of play, so the "frames" below are
  those seconds' averages, not single frames; the spike section uses the layer's
  own per-submission warnings, which are single events.

.PARAMETER Log
  The log to read. Default: whatever logs\latest.txt names.

.PARAMETER SkipSeconds
  Seconds of the run left out of the medians: shader translation, the pipeline
  library and the first streaming of the world make the opening unlike play.
  Default 120.

.PARAMETER Out
  Also write the summary to this file (markdown).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File profile-drae.ps1
  powershell -ExecutionPolicy Bypass -File profile-drae.ps1 -Log logs\drae-bar-rt-153012.err.log -Out runs\bar-rt.md
#>
[CmdletBinding()]
param(
  [string] $Log,
  [int]    $SkipSeconds = 120,
  [string] $Out
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not $Log) {
  $latest = Join-Path $here 'logs\latest.txt'
  if (-not (Test-Path $latest)) { throw "No -Log given and no $latest" }
  $Log = (Get-Content $latest -Raw).Trim()
}
if (-not (Test-Path $Log)) { throw "No such log: $Log" }
$Log = (Resolve-Path $Log).Path

# ---------------------------------------------------------------- the lines
# One pass: the log is tens of MB, so keep the few lines wanted and drop the rest.
$want = @(
  ' fps, ',
  'list calls translated by threads of ours',
  "the card's clock per frame",
  "host copies in the card's memory:",
  'the relay per frame',
  'landing per frame',
  'landed per frame',
  'windows outside buffers, by where',
  'shader writes looked for on the CPU',
  'slow host submission',
  'slow list command,',
  'submissions prepared by a thread of ours',
  'dynamic slots, more',
  'fetched pieces checked per frame',
  'preparing per frame, the submitting thread'
)
# The colours the layer writes when it does not see a file on its stderr
# (a cmd redirect gets them, a PowerShell one does not): they sit around
# the timestamp and the module, so a line keeps none of them here.
$ansi = [regex]'\x1b\[[0-9;]*m'
$kept = New-Object 'System.Collections.Generic.List[string]'
# Shared, so a run still being played can be summarised while it writes.
$stream = [System.IO.File]::Open($Log, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
$reader = New-Object System.IO.StreamReader($stream)
try {
  while ($null -ne ($line = $reader.ReadLine())) {
    foreach ($w in $want) {
      if ($line.Contains($w)) { $kept.Add($ansi.Replace($line, '')); break }
    }
  }
} finally {
  $reader.Dispose()
  $stream.Dispose()
}

if ($kept.Count -eq 0) { throw "Nothing recognisable in $Log - was it a victory-drae run?" }

function Get-Stamp([string] $line) {
  # 2026-11-26T14:31:18.594230Z
  $m = [regex]::Match($line, '^(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)\.(\d+)Z')
  if (-not $m.Success) { return $null }
  # The date too, so that a run across midnight does not read as 86,399 s
  # long and throw the -SkipSeconds window away (run 235655 did).
  $day = [datetime]::new([int]$m.Groups[1].Value, [int]$m.Groups[2].Value, [int]$m.Groups[3].Value)
  $t = [double]$m.Groups[4].Value * 3600 + [double]$m.Groups[5].Value * 60 + [double]$m.Groups[6].Value
  return ($day.Ticks / 10000000) + $t + [double]("0." + $m.Groups[7].Value)
}

$stamps = @()
foreach ($l in $kept) {
  $s = Get-Stamp $l
  if ($null -ne $s) { $stamps += $s }
}
if ($stamps.Count -eq 0) { throw "No timestamps in $Log" }
$firstAt = ($stamps | Measure-Object -Minimum).Minimum
$lastAt = ($stamps | Measure-Object -Maximum).Maximum
$runSeconds = $lastAt - $firstAt
$script:from = $firstAt + $SkipSeconds

function Select-Lines([string] $needle, [switch] $Settled) {
  $out = New-Object 'System.Collections.Generic.List[string]'
  foreach ($l in $kept) {
    if (-not $l.Contains($needle)) { continue }
    if ($Settled) {
      $s = Get-Stamp $l
      if ($null -eq $s -or $s -lt $script:from) { continue }
    }
    $out.Add($l)
  }
  return $out
}

# Every capture group of $pattern, from every line holding $needle.
function Get-Series([string] $needle, [string] $pattern, [switch] $Settled) {
  $rows = @()
  foreach ($l in (Select-Lines $needle -Settled:$Settled)) {
    $m = [regex]::Match($l, $pattern)
    if ($m.Success) {
      $row = @()
      for ($i = 1; $i -lt $m.Groups.Count; $i++) { $row += [double]$m.Groups[$i].Value }
      $rows += , $row
    }
  }
  return $rows
}

function Get-Column($rows, [int] $i) {
  $v = @()
  foreach ($r in $rows) { $v += $r[$i] }
  return , $v
}

function Get-Quantile($values, [double] $q) {
  if (-not $values -or $values.Count -eq 0) { return $null }
  $s = @($values | Sort-Object)
  $at = [int][math]::Floor($q * ($s.Count - 1) + 0.5)
  return $s[$at]
}

function Format-Number($v, [int] $digits = 1) {
  if ($null -eq $v) { return '-' }
  return ([math]::Round([double]$v, $digits)).ToString()
}

# median / p90 / worst of a column, as one cell
function Format-Spread($values, [int] $digits = 1) {
  if (-not $values -or $values.Count -eq 0) { return '-' }
  $med = Get-Quantile $values 0.5
  $p90 = Get-Quantile $values 0.9
  $max = ($values | Measure-Object -Maximum).Maximum
  return (Format-Number $med $digits) + ' / ' + (Format-Number $p90 $digits) + ' / ' + (Format-Number $max $digits)
}

# ------------------------------------------------------------------ frames
$frameRx = '([\d.]+) fps, ([\d.]+) ms/frame \(([\d.]+) preparing: copies ([\d.]+) ms ([\d.]+) MB, textures ([\d.]+) ms ([\d.]+) MB; ([\d.]+) on the GPU; ([\d.]+) copying back ([\d.]+) MB\), ([\d.]+) draws/frame, dropped ([\d.]+) without a host pipeline \(([\d.]+) being made\) and ([\d.]+) indirect; heap buffers ([\d.]+) passes ([\d.]+) ms'
$framesAll = Get-Series ' fps, ' $frameRx
$frames = Get-Series ' fps, ' $frameRx -Settled
if ($frames.Count -eq 0) {
  Write-Warning "No frame line after the first $SkipSeconds s; using the whole run."
  $frames = $framesAll
  $script:from = $firstAt
}
if ($frames.Count -eq 0) { throw "No frame-rate line parsed in $Log" }

$col = @{}
$names = @('fps', 'ms', 'prep', 'copyms', 'copymb', 'texms', 'texmb', 'gpu', 'backms', 'backmb', 'draws', 'dropped', 'making', 'indirect', 'heappass', 'heapms')
for ($i = 0; $i -lt $names.Count; $i++) { $col[$names[$i]] = Get-Column $frames $i }

# --------------------------------------------------------------- the rest
$thin = Get-Series 'list calls translated' 'VICTORY_THIN\): ([\d.]+) in ([\d.]+) ms \(all threads\), ([\d.]+) lists, ([\d.]+) pieces' -Settled
$clock = Get-Series "the card's clock per frame" "nothing of ours queued ([\d.]+) ms \(the card about ([\d.]+)% busy by this count\), ([\d.]+) ms of that while the submitting thread was working .*?the submitting thread stood waiting for the card ([\d.]+) ms" -Settled
$card = Get-Series "host copies in the card's memory:" "([\d.]+) copies ([\d.]+) MB,.*?per frame: ([\d.]+) MB sent up in ([\d.]+) ranges, ([\d.]+) pieces compared on the card, ([\d.]+) of them fetched \(([\d.]+) MB\) with ([\d.]+) more waits for the card \(([\d.]+) ms\)" -Settled
$relay = Get-Series 'the relay per frame' "waited ([\d.]+) ms for the card with nothing to land and ([\d.]+) ms for fetches of card copies' pieces, and was ([\d.]+) ms landing in all" -Settled
$land = Get-Series 'landing per frame' 'which pieces to fetch ([\d.]+) ms.*?looking through copies in the PC.s memory ([\d.]+) ms beside them and ([\d.]+) ms more; merging what was fetched ([\d.]+) ms \(([\d.]+) ms on all threads, ([\d.]+) jobs\), merging the others ([\d.]+) ms.*?opening or noting guards; then ranges done with ([\d.]+) ms, read-backs and lists ([\d.]+), the title told ([\d.]+), the rest ([\d.]+)' -Settled
$landed = Get-Series 'landed per frame' 'landed per frame: ([\d.]+) pieces, ([\d.]+) with a new byte; of their ([\d.]+) pages of 4 KiB ([\d.]+) with a new byte \(([\d.]+) MB\), ([\d.]+) MB in blocks of 32 bytes with one, ([\d.]+) MB put as dwords' -Settled
$cpuback = Get-Series 'shader writes looked for on the CPU' 'per frame: ([\d.]+) MB compared in ([\d.]+) ranges of windows outside buffers \(([\d.]+) MB snapshotted\), ([\d.]+) MB in ([\d.]+) ranges of title buffers' -Settled
$slots = Get-Series 'dynamic slots, more' 'more: ([\d.]+) slots taken per frame of ([\d.]+) \(([\d.]+) free now\), ([\d.]+) tables took a run' -Settled
# What VICTORY_FETCH_PARTS left out of the fetch, where the build reports it.
$partsRx = 'of the pieces fetched ([\d.]+) MB lay in them whole and ([\d.]+) MB (?:came down|was wanted of them) \(VICTORY_FETCH_PARTS=([\d]+)'
# Whether the torn check is running: it hashes each fetched piece whole, so
# the pieces still come down whole and only the merge is narrowed.
$checked = Get-Series 'fetched pieces checked per frame' 'VICTORY_FETCH_CHECK\): ([\d.]+), of which ([\d.]+) were not what their flight ended' -Settled
$parted = Get-Series 'of the pieces fetched' $partsRx -Settled
# Where the submitting thread's preparing went (always on since the parts work).
$prepRx = 'preparing per frame, the submitting thread: ([\d.]+) ms asking the write watch \(([\d.]+) of them finding what to ask\), ' +
  '([\d.]+) putting the title.s bytes under flights. writes, ([\d.]+) in the lists. steps \(refreshes ([\d.]+) in ([\d.]+) flushes of ' +
  '([\d.]+), ([\d.]+) of them known to copy nothing, ([\d.]+) on several threads, ([\d.]+) one at a time in ([\d.]+) before shader ' +
  'writes; copies and fills ([\d.]+), the refreshes before copies among them\), ([\d.]+) snapshots, ([\d.]+) for copies in the card.s ' +
  'memory, ([\d.]+) sending the lists \(VICTORY_LIGHT_REFRESH is \w+\); ahead of these ([\d.]+) waiting for the submission lock, ' +
  '([\d.]+) planning steps done on the card, ([\d.]+) waiting for flights, ([\d.]+) the heaps. records and what is swept; at the end ' +
  '([\d.]+) waiting for the heaps. records'
$prep = Get-Series 'preparing per frame, the submitting thread' $prepRx -Settled
# Name -> column. The parts that add up to "preparing" come first; the two
# shown under the lists' steps are inside it, so they are marked.
$prepParts = @(
  @{ Name = 'the lists'' steps'; Value = 3 },
  @{ Name = '- of those: copies and fills'; Value = 11 },
  @{ Name = '- of those: refreshes'; Value = 4 },
  @{ Name = 'asking the write watch'; Value = 0 },
  @{ Name = 'copies in the card''s memory'; Value = 13 },
  @{ Name = 'sending the lists'; Value = 14 },
  @{ Name = 'snapshots'; Value = 12 },
  @{ Name = "title bytes under flights' writes"; Value = 2 },
  @{ Name = 'waiting for the submission lock'; Value = 15 },
  @{ Name = 'planning steps done on the card'; Value = 16 },
  @{ Name = 'waiting for flights'; Value = 17 },
  @{ Name = "the heaps' records and what is swept"; Value = 18 },
  @{ Name = "waiting for the heaps' records at the end"; Value = 19 }
)
# The columns that do not double-count (the two sub-parts are left out).
$prepTop = @(3, 0, 13, 14, 12, 2, 15, 16, 17, 18, 19)
$partsOn = $false
if ($parted.Count -gt 0) { $partsOn = (Get-Quantile (Get-Column $parted 2) 0.5) -ge 1 }

# the windows census: the last line, and its numbers
$winLines = Select-Lines 'windows outside buffers, by where'
$windows = $null
if ($winLines.Count -gt 0) { $windows = $winLines[$winLines.Count - 1] }
$winRx = 'VICTORY_WINDOWS_BAR_MB=([\d.]+)\): ([\d.]+) in the title.s own memory in place \(([\d.]+) MB, ([\d.]+)%\), ([\d.]+) in card memory the CPU maps \(([\d.]+) MB, ([\d.]+)%; ([\d.]+) MB of them alive now, at most ([\d.]+) MB at one time of the ([\d.]+) MB allowed\), ([\d.]+) in the card.s own memory with a CPU side \(([\d.]+) MB, ([\d.]+)%\), ([\d.]+) in the PC.s memory \(([\d.]+) MB, ([\d.]+)%\); the BAR turned down ([\d.]+) for want of room under the cap \(([\d.]+) MB.*?and ([\d.]+) with the card.s budget tight \(([\d.]+) MB.*?and ([\d.]+) buffers the host refused; filling windows per frame: ([\d.]+) ms ([\d.]+) MB over the bus into the BAR, ([\d.]+) ms ([\d.]+) MB'
$winNums = Get-Series 'windows outside buffers, by where' $winRx
$win = $null
if ($winNums.Count -gt 0) { $win = $winNums[$winNums.Count - 1] }

# spikes: the layer's own per-submission warnings
$slowSubs = @()
foreach ($l in (Select-Lines 'slow host submission' -Settled)) {
  $m = [regex]::Match($l, 'ms=(\d+)')
  if ($m.Success) { $slowSubs += [double]$m.Groups[1].Value }
}
$slowLists = @()
foreach ($l in (Select-Lines 'slow list command,' -Settled)) {
  $m = [regex]::Match($l, 'slow list command, ([\d.]+) ms')
  if ($m.Success) { $slowLists += [double]$m.Groups[1].Value }
}

# frame-line spikes: seconds whose mean frame was far above the run's median
$spikey = 0
$medMs = Get-Quantile $col['ms'] 0.5
if ($medMs) {
  foreach ($v in $col['ms']) {
    if ($v -gt 2 * $medMs) { $spikey++ }
  }
}

# ------------------------------------------------------------------ report
$r = New-Object 'System.Collections.Generic.List[string]'
function Say([string] $s) { $r.Add($s) }

$logName = Split-Path -Leaf $Log
Say "# victory-drae run: $logName"
Say ''
Say ("Log: " + $Log)
Say ("Run: {0:n0} s of log, {1} frame lines; medians over the {2} after the first {3} s." -f $runSeconds, $framesAll.Count, $frames.Count, $SkipSeconds)
Say ''
$sidecar = [regex]::Replace($Log, '\.err\.log$', '.log')
if (Test-Path $sidecar) {
  foreach ($h in (Get-Content $sidecar -TotalCount 40)) {
    if ($h -match '^(console|title|build|mapped at)\s') { Say ('    ' + $h) }
  }
  Say ''
}

Say '## Where the frame goes'
Say ''
Say 'Each cell is median / p90 / worst of the per-second lines. "Waiting for the card" is the'
Say 'submitting thread standing still, so it overlaps nothing else here; the rest is work it did.'
Say ''
Say '| per frame | ms (med/p90/worst) | MB |'
Say '|---|---|---|'
Say ('| frame | ' + (Format-Spread $col['ms']) + ' | - |')
Say ('| fps | ' + (Format-Spread $col['fps'] 2) + ' | - |')
Say ('| preparing, all of it | ' + (Format-Spread $col['prep']) + ' | - |')
Say ('| - copies up | ' + (Format-Spread $col['copyms']) + ' | ' + (Format-Spread $col['copymb']) + ' |')
Say ('| - textures | ' + (Format-Spread $col['texms']) + ' | ' + (Format-Spread $col['texmb']) + ' |')
Say ('| waiting for the card | ' + (Format-Spread $col['gpu']) + ' | - |')
Say ('| copying back | ' + (Format-Spread $col['backms']) + ' | ' + (Format-Spread $col['backmb']) + ' |')
Say ('| heap buffer passes | ' + (Format-Spread $col['heapms']) + ' | - |')
if ($thin.Count -gt 0) {
  Say ('| list calls translated (all threads) | ' + (Format-Spread (Get-Column $thin 1)) + ' | ' + (Format-Spread (Get-Column $thin 0) 0) + ' calls |')
}
if ($clock.Count -gt 0) {
  Say ('| the card itself idle | ' + (Format-Spread (Get-Column $clock 0)) + ' | ' + (Format-Spread (Get-Column $clock 1) 0) + '% busy |')
}
Say ''
Say ('Draws: ' + (Format-Spread $col['draws'] 0) + ' a frame; dropped without a host pipeline ' + (Format-Spread $col['dropped'] 0) + ', indirect ' + (Format-Spread $col['indirect'] 0) + '.')
if ($slots.Count -gt 0) {
  Say ('Descriptor slots: ' + (Format-Spread (Get-Column $slots 0) 0) + ' taken a frame, ' + (Format-Spread (Get-Column $slots 3) 0) + ' tables refused for want of them.')
}
Say ''

Say '## Preparing, in its parts'
Say ''
if ($prep.Count -gt 0) {
  Say 'All of this is the submitting thread, serial: it is the biggest block of the frame.'
  Say ''
  Say '| part of preparing | ms (med/p90/worst) |'
  Say '|---|---|'
  $rows = @()
  foreach ($p in $prepParts) {
    $vals = Get-Column $prep $p.Value
    $rows += [pscustomobject]@{ Name = $p.Name; Med = (Get-Quantile $vals 0.5); Cell = (Format-Spread $vals 2) }
  }
  foreach ($row in ($rows | Sort-Object -Property Med -Descending)) {
    Say ('| ' + $row.Name + ' | ' + $row.Cell + ' |')
  }
  $named = 0
  foreach ($p in $prepTop) { $named += [double](Get-Quantile (Get-Column $prep $p) 0.5) }
  $prepTotal = Get-Quantile $col['prep'] 0.5
  Say ''
  Say ('Their medians add to ' + (Format-Number $named 1) + ' ms against the ' + (Format-Number $prepTotal 1) +
    ' ms median the frame line calls preparing. The two need not agree: a median of sums is not the ' +
    'sum of medians, and the parts peak in different seconds. Read the ranking, not the total.')
}
else {
  Say 'No preparing split in this log: the build is older than the always-on split.'
}
Say ''

Say '## Copying back, in its parts'
Say ''
if ($relay.Count -gt 0) {
  Say ('The landing thread: ' + (Format-Spread (Get-Column $relay 2)) + ' ms landing in all, of which ' + (Format-Spread (Get-Column $relay 1)) + ' ms waiting for fetches of card copies.')
}
if ($land.Count -gt 0) {
  Say ''
  Say '| part of landing | ms (med/p90/worst) |'
  Say '|---|---|'
  Say ('| picking which pieces to fetch | ' + (Format-Spread (Get-Column $land 0) 2) + ' |')
  Say ('| looking through PC-memory copies | ' + (Format-Spread (Get-Column $land 1) 2) + ' |')
  Say ('| merging what was fetched | ' + (Format-Spread (Get-Column $land 3) 2) + ' |')
  Say ('| merging the others | ' + (Format-Spread (Get-Column $land 6) 2) + ' |')
  Say ('| ranges done with | ' + (Format-Spread (Get-Column $land 7) 2) + ' |')
  Say ('| read-backs and lists | ' + (Format-Spread (Get-Column $land 8) 2) + ' |')
  Say ('| the rest | ' + (Format-Spread (Get-Column $land 10) 2) + ' |')
}
if ($card.Count -gt 0) {
  Say ''
  Say ('Card copies: ' + (Format-Number (Get-Quantile (Get-Column $card 0) 0.5) 0) + ' of them, ' +
    (Format-Number (Get-Quantile (Get-Column $card 1) 0.5) 0) + ' MB. A frame sends up ' + (Format-Spread (Get-Column $card 2)) +
    ' MB in ' + (Format-Spread (Get-Column $card 3) 0) + ' ranges, compares ' + (Format-Spread (Get-Column $card 4) 0) +
    ' pieces on the card and fetches ' + (Format-Spread (Get-Column $card 5) 0) + ' of them (' + (Format-Spread (Get-Column $card 6)) +
    ' MB) in ' + (Format-Spread (Get-Column $card 7)) + ' extra waits costing ' + (Format-Spread (Get-Column $card 8)) + ' ms.')
}
if ($landed.Count -gt 0) {
  $fetchedMb = $null
  if ($card.Count -gt 0) { $fetchedMb = Get-Quantile (Get-Column $card 6) 0.5 }
  $newMb = Get-Quantile (Get-Column $landed 4) 0.5
  $blockMb = Get-Quantile (Get-Column $landed 5) 0.5
  $dwordMb = Get-Quantile (Get-Column $landed 6) 0.5
  Say ''
  Say ('Of what comes down: ' + (Format-Number $fetchedMb) + ' MB fetched, ' + (Format-Number $newMb) +
    ' MB in 4 KiB pages that hold a new byte, ' + (Format-Number $blockMb) + ' MB in 32-byte blocks that hold one, ' +
    (Format-Number $dwordMb 2) + ' MB finally written into title memory as dwords.')
  if ($fetchedMb -and $newMb -gt 0) {
    $at4k = $fetchedMb / $newMb
    $at32 = $fetchedMb / [math]::Max($blockMb, 0.01)
    if ($partsOn) {
      # The fetch is already cut to 4 KiB, so what is left is finer than that.
      Say ('Of what still comes down, ' + (Format-Number (100 / $at4k) 0) +
        '% of the 4 KiB pages hold a new byte, so little is left to win at that size; a mask of 32-byte blocks ' +
        'would fetch another ' + (Format-Number $at32 2) + 'x less.')
    }
    else {
      Say ('Flagging the compare at 4 KiB instead of 64 KiB would fetch ' + (Format-Number $at4k 2) +
        'x less; at 32 bytes, ' + (Format-Number $at32 2) + 'x less. That is the size of the copy-back asymmetry.')
    }
  }
}
if ($parted.Count -gt 0) {
  $on = $partsOn
  $wholeMb = Get-Quantile (Get-Column $parted 0) 0.5
  $downMb = Get-Quantile (Get-Column $parted 1) 0.5
  Say ''
  if ($on -and $wholeMb -gt 0) {
    $cut = $wholeMb / [math]::Max($downMb, 0.01)
    Say ('**VICTORY_FETCH_PARTS is on:** the flagged pieces hold ' + (Format-Number $wholeMb) + ' MB and ' +
      (Format-Number $downMb) + ' MB of that was wanted, a ' + (Format-Number $cut 2) + 'x cut.')
    if ($checked.Count -gt 0) {
      $hashed = Get-Quantile (Get-Column $checked 0) 0.5
      $torn = Get-Quantile (Get-Column $checked 1) 0.5
      Say ('But VICTORY_FETCH_CHECK is on as well (' + (Format-Number $hashed 0) + ' pieces hashed a frame, ' +
        (Format-Number $torn 2) + ' of them caught mid-flight), and it hashes each piece whole: **the cut is in the ' +
        'merge, not in the bus traffic.** The fetch still moves the whole ' + (Format-Number $wholeMb) + ' MB. ' +
        'Dropping VICTORY_FETCH_CHECK would cut the fetch too, at the price of those pieces caught mid-flight.')
    }
    else {
      Say 'Nothing hashes the pieces whole, so the cut is in the bus traffic as well as the merge.'
    }
  }
  elseif ($on) {
    Say 'VICTORY_FETCH_PARTS is on but narrowed nothing: either no piece was flagged, or every piece changed throughout.'
  }
  else {
    Say ('VICTORY_FETCH_PARTS is off: the pieces fetched held ' + (Format-Number $wholeMb) +
      ' MB and all of it came down. Pass "parts" to run-drae.cmd to fetch only the 4 KiB that changed.')
  }
}
if ($cpuback.Count -gt 0) {
  Say ''
  Say ("Looked through on the CPU instead: " + (Format-Spread (Get-Column $cpuback 3)) + " MB of title buffers' copies in " +
    (Format-Spread (Get-Column $cpuback 4) 0) + ' ranges, ' + (Format-Spread (Get-Column $cpuback 0)) + ' MB of windows.')
}
Say ''

Say '## Windows: how much of memory is one memory'
Say ''
if ($win) {
  $cap = $win[0]
  $placed = $win[1]; $placedmb = $win[2]; $placedpc = $win[3]
  $bar = $win[4]; $barmb = $win[5]; $barpc = $win[6]; $alive = $win[7]; $most = $win[8]; $allowed = $win[9]
  $oncard = $win[10]; $oncardmb = $win[11]; $oncardpc = $win[12]
  $inpc = $win[13]; $inpcmb = $win[14]; $inpcpc = $win[15]
  $noroom = $win[16]; $noroommb = $win[17]; $nobudget = $win[18]; $nobudgetmb = $win[19]
  $refusedbuf = $win[20]
  $barms = $win[21]; $barfillmb = $win[22]; $pcms = $win[23]; $pcfillmb = $win[24]
  Say ('VICTORY_WINDOWS_BAR_MB = ' + $cap + ' MB.')
  Say ''
  Say '| windows made | number | MB | share of windows'' bytes |'
  Say '|---|---|---|---|'
  Say ("| in the title's own memory, in place | " + $placed + ' | ' + (Format-Number $placedmb 0) + ' | ' + (Format-Number $placedpc 0) + '% |')
  Say ('| in card memory the CPU maps (the BAR) | ' + $bar + ' | ' + (Format-Number $barmb 0) + ' | ' + (Format-Number $barpc 0) + '% |')
  Say ("| in the card's own memory, CPU side beside it | " + $oncard + ' | ' + (Format-Number $oncardmb 0) + ' | ' + (Format-Number $oncardpc 0) + '% |')
  Say ("| in the PC's memory | " + $inpc + ' | ' + (Format-Number $inpcmb 0) + ' | ' + (Format-Number $inpcpc 0) + '% |')
  Say ''
  Say ('Alive in the BAR now ' + (Format-Number $alive 0) + ' MB; most at one time ' + (Format-Number $most 0) + ' MB of the ' + $allowed + ' MB allowed.')
  Say ('Filling windows costs ' + (Format-Number $barms 2) + ' ms a frame for ' + (Format-Number $barfillmb 2) +
    " MB over the bus into the BAR, and " + (Format-Number $pcms 2) + ' ms for ' + (Format-Number $pcfillmb 2) + " MB in the PC's memory.")
  Say ''
  if ($noroom -gt 0) {
    $try = [int]([math]::Ceiling(($most + $noroommb) / 512) * 512)
    Say ('**A larger cap would take more:** ' + $noroom + ' windows (' + (Format-Number $noroommb 0) +
      ' MB) were turned down for want of room under it. Try VICTORY_WINDOWS_BAR_MB=' + $try + '.')
  }
  else {
    Say 'Nothing was turned down for want of room under the cap, so raising it would change nothing.'
  }
  if ($nobudget -gt 0) {
    Say ('' + $nobudget + ' windows (' + (Format-Number $nobudgetmb 0) +
      " MB) were turned down with the card's budget tight: the card is full, and those want card memory freed, not a larger cap.")
  }
  if ($refusedbuf -gt 0) { Say ('' + $refusedbuf + ' buffers the host refused outright.') }
}
elseif ($windows) {
  Say 'The windows line is there but did not parse. Verbatim:'
  Say ''
  Say ('    ' + $windows)
}
else {
  Say 'No windows line in this log: either the build is older than the census, or no window was made.'
}
Say ''

Say '## Spikes'
Say ''
Say ("Seconds whose mean frame was over twice the run's median (" + (Format-Number $medMs) + ' ms): ' + $spikey + ' of ' + $frames.Count + '.')
if ($slowSubs.Count -gt 0) {
  Say ('Submissions the layer itself called slow (over 250 ms): ' + $slowSubs.Count + ', median ' +
    (Format-Number (Get-Quantile $slowSubs 0.5) 0) + ' ms, worst ' + (Format-Number ($slowSubs | Measure-Object -Maximum).Maximum 0) + ' ms.')
}
else {
  Say 'No submission took over 250 ms: nothing the layer calls slow.'
}
if ($slowLists.Count -gt 0) {
  Say ('Slow list commands named (diagnostics build only): ' + $slowLists.Count + ', worst ' +
    (Format-Number ($slowLists | Measure-Object -Maximum).Maximum 0) + ' ms.')
}
Say ''

Say '## The lines themselves (the last of each)'
Say ''
$lastOf = @('list calls translated', "the card's clock per frame", "host copies in the card's memory:",
  'the relay per frame', 'landing per frame', 'landed per frame',
  'windows outside buffers, by where', 'shader writes looked for on the CPU')
foreach ($w in $lastOf) {
  $ls = Select-Lines $w
  if ($ls.Count -gt 0) {
    $text = $ls[$ls.Count - 1]
    $cut = $text.IndexOf('  INFO ')
    if ($cut -ge 0) { $text = $text.Substring($cut + 7) }
    Say ('    ' + $text)
    Say ''
  }
}

$report = ($r -join "`r`n")
Write-Output $report
if ($Out) {
  $dir = Split-Path -Parent $Out
  if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
  $report | Out-File -FilePath $Out -Encoding utf8
  Write-Output ''
  Write-Output "Written to $Out"
}
