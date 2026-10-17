# Reproduces docs/decisions/002-disabled-servers.md: drives the REAL Claude Code terminal UI (through spikes/pty-drive)
# in a THROWAWAY CLAUDE_CONFIG_DIR with a dummy API key and seeded onboarding, so no login or browser flow starts.
# Build the driver first:  cargo build --manifest-path docs/decisions/spikes/pty-drive/Cargo.toml
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$driver = Join-Path $here "pty-drive\target\debug\pty-drive.exe"
if (-not (Test-Path $driver)) { throw "build the pty-drive spike first: $driver" }
$claude = Join-Path $env:USERPROFILE ".local\bin\claude.exe"
$root = Join-Path $env:TEMP "klefki-spike-disable-repro"
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
$cfg = Join-Path $root "cfg"; $proj = Join-Path $root "My Project"
New-Item -ItemType Directory $cfg, $proj | Out-Null
$json = Join-Path $cfg ".claude.json"
$key = "sk-ant-api03-" + ("DUMMY" * 16) + "-DUMMYAA"
$env:CLAUDE_CONFIG_DIR = $cfg
Set-Location $proj
& $claude mcp add -s user --transport http probeU https://example.com/u | Out-Null
& $claude mcp add -s local --transport http probeL https://example.com/l | Out-Null
[IO.File]::WriteAllText("$proj\.mcp.json", '{"mcpServers":{"projP":{"type":"http","url":"https://example.com/p"}}}', (New-Object Text.UTF8Encoding $false))

# Seed: onboarding done, dark theme, the dummy key pre-approved, folder trusted, project server pre-approved.
$c = Get-Content -Raw $json | ConvertFrom-Json
$c | Add-Member -NotePropertyName hasCompletedOnboarding -NotePropertyValue $true -Force
$c | Add-Member -NotePropertyName theme -NotePropertyValue "dark" -Force
$c | Add-Member -NotePropertyName customApiKeyResponses -NotePropertyValue ([pscustomobject]@{ approved = @($key.Substring($key.Length - 20)); rejected = @() }) -Force
$entry = ($c.projects.PSObject.Properties | Select-Object -First 1).Value
$entry.hasTrustDialogAccepted = $true
$entry.enabledMcpjsonServers = @("projP")
[IO.File]::WriteAllText($json, ($c | ConvertTo-Json -Depth 20), (New-Object Text.UTF8Encoding $false))

function State($label) {
  $p = ((Get-Content -Raw $json | ConvertFrom-Json).projects.PSObject.Properties | Select-Object -First 1).Value
  $sl = Join-Path $proj ".claude\settings.local.json"
  "{0,-22} claude.json project entry: disabledMcpServers=[{1}] enabledMcpjsonServers=[{2}] disabledMcpjsonServers=[{3}] | settings.local.json: {4}" -f `
    $label, ($p.disabledMcpServers -join ","), ($p.enabledMcpjsonServers -join ","), ($p.disabledMcpjsonServers -join ","),
    $(if (Test-Path $sl) { (Get-Content -Raw $sl) -replace "\s+", " " } else { "(absent)" })
}
function Toggle($verb, $name) {
  $steps = @("sleep=7000", "send=/mcp $verb $name", "sleep=800", "send=\r", "sleep=2500", "dump")
  $job = Start-Job { param($driver, $proj, $claude, $cfg, $key, $steps) & $driver $proj $claude --env "CLAUDE_CONFIG_DIR=$cfg" --env "ANTHROPIC_API_KEY=$key" -- @steps 2>&1 } `
    -ArgumentList $driver, $proj, $claude, $cfg, $key, $steps
  if (Wait-Job $job -Timeout 90) {
    Receive-Job $job | ForEach-Object { ($_ -replace "[^\x20-\x7e]", "").Trim() } | Where-Object { $_ -match "^MCP server" }
  } else { Stop-Job $job; "(driver timed out)" }
  Remove-Job $job -Force
}

State "start"
Toggle "disable" "probeU"; State "after disable probeU"
Toggle "disable" "probeL"; State "after disable probeL"
Toggle "disable" "projP";  State "after disable projP"
Toggle "enable" "probeU";  State "after enable probeU"
Toggle "enable" "probeL";  State "after enable probeL"
Toggle "enable" "projP";   State "after enable projP"
"project key as Claude wrote it: " + ((Get-Content -Raw $json | ConvertFrom-Json).projects.PSObject.Properties | Select-Object -First 1).Name
$env:CLAUDE_CONFIG_DIR = $null
Set-Location $env:TEMP
Remove-Item -Recurse -Force $root
