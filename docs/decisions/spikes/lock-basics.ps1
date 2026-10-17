# Reproduces the first half of docs/decisions/001-claude-json-locking.md against a THROWAWAY CLAUDE_CONFIG_DIR.
# Never touches the real ~/.claude.json. Needs the native claude at ~/.local/bin/claude.exe.
#   (a) the lock is a directory, (b) an outside edit survives a Claude write, (c) Claude does not wait for a lock we hold
# The longer "hold the lock with a fresh mtime" experiment is lock-hold.ps1.
$ErrorActionPreference = "Stop"
$root = Join-Path $env:TEMP "klefki-spike-lock-basics"
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
New-Item -ItemType Directory $root | Out-Null
$cfg = Join-Path $root "cfg"; New-Item -ItemType Directory $cfg | Out-Null
$claude = Join-Path $env:USERPROFILE ".local\bin\claude.exe"
$json = Join-Path $cfg ".claude.json"; $lock = "$json.lock"
$env:CLAUDE_CONFIG_DIR = $cfg
Set-Location $root

function Run-Claude([string[]]$argv, [switch]$PollLock) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $claude -ArgumentList $argv -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput "$root\o.txt" -RedirectStandardError "$root\e.txt"
  $dirPolls = 0; $filePolls = 0
  while (-not $p.HasExited) {
    if ($PollLock) {
      if ([IO.Directory]::Exists($lock)) { $dirPolls++ } elseif ([IO.File]::Exists($lock)) { $filePolls++ }
    } else { Start-Sleep -Milliseconds 20 }
  }
  $p.WaitForExit()
  [pscustomobject]@{ Ms = $sw.ElapsedMilliseconds; DirPolls = $dirPolls; FilePolls = $filePolls }
}

$null = Run-Claude @("mcp", "add", "-s", "user", "--transport", "http", "warm", "https://example.com/mcp")

"== (a) is the lock a directory?"
$r = Run-Claude @("mcp", "add", "-s", "user", "--transport", "http", "probeA", "https://example.com/a") -PollLock
"command {0} ms; lock seen as directory in {1} polls, as file in {2} polls; left behind: {3}" -f $r.Ms, $r.DirPolls, $r.FilePolls, (Test-Path $lock)

"== (b) does an outside edit survive a Claude write?"
$text = Get-Content -Raw $json
$edited = $text.TrimEnd().TrimEnd('}').TrimEnd() + ",`n  `"zzOutsideKey`": { `"f`": 1.5 }`n}"
[IO.File]::WriteAllText($json, $edited, (New-Object Text.UTF8Encoding $false))
$null = Run-Claude @("mcp", "add", "-s", "user", "--transport", "http", "probeB", "https://example.com/b")
$after = Get-Content -Raw $json | ConvertFrom-Json
"outside key kept: {0}; float kept: {1}; probeB written: {2}; last top-level key: {3}" -f ($null -ne $after.zzOutsideKey), $after.zzOutsideKey.f, ($null -ne $after.mcpServers.probeB), ($after.PSObject.Properties.Name | Select-Object -Last 1)
"file ends with newline: {0}" -f ((Get-Content -Raw $json).EndsWith("`n"))

"== (c) does Claude wait while we hold the lock for 3000 ms?"
$base = Run-Claude @("mcp", "add", "-s", "user", "--transport", "http", "probeBase", "https://example.com/base")
"baseline command: $($base.Ms) ms"
[IO.Directory]::CreateDirectory($lock) | Out-Null
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $claude -ArgumentList @("mcp", "add", "-s", "user", "--transport", "http", "probeC", "https://example.com/c") `
  -PassThru -WindowStyle Hidden -RedirectStandardOutput "$root\o.txt" -RedirectStandardError "$root\e.txt"
Start-Sleep -Milliseconds 3000
$written = $null -ne (Get-Content -Raw $json | ConvertFrom-Json).mcpServers.probeC
"after 3000 ms of holding the lock: probeC already written = $written; claude exited = $($p.HasExited)"
Remove-Item -Recurse -Force $lock
$p.WaitForExit()
$env:CLAUDE_CONFIG_DIR = $null
Set-Location $env:TEMP
Remove-Item -Recurse -Force $root
