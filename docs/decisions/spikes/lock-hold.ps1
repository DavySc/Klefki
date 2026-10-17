$ErrorActionPreference = "Stop"
$root = Join-Path $env:TEMP "klefki-spike-lock3"
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
New-Item -ItemType Directory $root | Out-Null
$cfg = Join-Path $root "cfg"; New-Item -ItemType Directory $cfg | Out-Null
$claude = Join-Path $env:USERPROFILE ".local\bin\claude.exe"
$json = Join-Path $cfg ".claude.json"; $lock = "$json.lock"
$env:CLAUDE_CONFIG_DIR = $cfg
Set-Location $root

function Start-Claude([string[]]$argv) {
  Start-Process -FilePath $claude -ArgumentList $argv -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput "$root\o.txt" -RedirectStandardError "$root\e.txt"
}

$p = Start-Claude @("mcp", "add", "-s", "user", "--transport", "http", "warm", "https://example.com/mcp")
$p.WaitForExit()

foreach ($hold in @(1500, 30000)) {
  Remove-Item -Recurse -Force $lock -ErrorAction SilentlyContinue
  [IO.Directory]::CreateDirectory($lock) | Out-Null
  $name = "p$hold"
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Claude @("mcp", "add", "-s", "user", "--transport", "http", $name, "https://example.com/$hold")
  $lastTouch = 0
  $wrote = $null
  while (-not $p.HasExited -and $sw.ElapsedMilliseconds -lt 32000) {
    Start-Sleep -Milliseconds 50
    if ($sw.ElapsedMilliseconds - $lastTouch -ge 4000) {
      (Get-Item $lock).LastWriteTime = Get-Date
      $lastTouch = $sw.ElapsedMilliseconds
    }
    if ($null -eq $wrote -and $sw.ElapsedMilliseconds -gt 300) {
      try {
        $names = (Get-Content -Raw $json | ConvertFrom-Json).mcpServers.PSObject.Properties.Name
        if ($names -contains $name) { $wrote = $sw.ElapsedMilliseconds }
      } catch { }
    }
  }
  $exited = $p.HasExited
  if (-not $exited) { $p.Kill() }
  $lockStillOurs = Test-Path $lock
  $err = (Get-Content -Raw "$root\e.txt")
  if ($null -eq $err) { $err = "" }
  Write-Output ("hold={0} ms (mtime kept fresh): claude exited={1} at {2} ms; entry first visible at {3} ms; our lock dir still there={4}; stderr='{5}'" -f $hold, $exited, $sw.ElapsedMilliseconds, $wrote, $lockStillOurs, ($err.Trim() -replace '\s+', ' '))
  Remove-Item -Recurse -Force $lock -ErrorAction SilentlyContinue
}
$env:CLAUDE_CONFIG_DIR = $null
