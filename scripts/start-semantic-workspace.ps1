param(
  [switch]$StopExisting
)

# Multi-repo note:
# The semantic API is single-repo per process. Each workspace project gets its
# own instance on a dedicated port. The token tracking dashboard (port 4319)
# reads from test_repo which is the primary indexed repo.
#
# Ports:
#   4317 - semantic API (test_repo / primary)
#   4319 - token tracking dashboard  -> <TOKEN_TRACKING_BASE_URL>
#   4321 - MCP bridge

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$tmp  = Join-Path $root ".tmp"
New-Item -ItemType Directory -Force $tmp | Out-Null

if ($StopExisting) {
  Get-Process api,mcp_bridge,token_tracking,cargo -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Seconds 1
}

# --- token: reuse or generate ---
$tokenFile = Join-Path $root ".semantic\mcp_bridge_token.txt"
if (-not $env:MCP_BRIDGE_TOKEN -or [string]::IsNullOrWhiteSpace($env:MCP_BRIDGE_TOKEN)) {
  if (Test-Path $tokenFile) {
    $env:MCP_BRIDGE_TOKEN = (Get-Content $tokenFile -Raw).Trim()
  } else {
    $env:MCP_BRIDGE_TOKEN = [guid]::NewGuid().ToString("N")
    New-Item -ItemType Directory -Force (Split-Path $tokenFile -Parent) | Out-Null
    Set-Content -Path $tokenFile -Value $env:MCP_BRIDGE_TOKEN -Encoding UTF8
  }
}
$env:SEMANTIC_BASE_URL = "<SEMANTIC_API_BASE_URL>"

# --- primary API (test_repo) ---
Start-Process -FilePath cargo `
  -ArgumentList "run -p api -- test_repo" `
  -WorkingDirectory $root `
  -RedirectStandardOutput (Join-Path $tmp "api.out.log") `
  -RedirectStandardError  (Join-Path $tmp "api.err.log") `
  -PassThru | Out-Null

# --- token tracking dashboard ---
Start-Process -FilePath cargo `
  -ArgumentList "run -p token_tracking -- test_repo" `
  -WorkingDirectory $root `
  -RedirectStandardOutput (Join-Path $tmp "token_tracking.out.log") `
  -RedirectStandardError  (Join-Path $tmp "token_tracking.err.log") `
  -PassThru | Out-Null

# --- MCP bridge ---
Start-Process -FilePath cargo `
  -ArgumentList "run -p mcp_bridge" `
  -WorkingDirectory $root `
  -RedirectStandardOutput (Join-Path $tmp "mcp.out.log") `
  -RedirectStandardError  (Join-Path $tmp "mcp.err.log") `
  -PassThru | Out-Null

function Wait-Health($url, $seconds) {
  for ($i = 0; $i -lt ($seconds * 2); $i++) {
    Start-Sleep -Milliseconds 500
    try {
      $r = Invoke-RestMethod -Method Get -Uri $url -TimeoutSec 1
      if ($r.status -eq "ok") { return $true }
    } catch {}
  }
  return $false
}

$apiReady   = Wait-Health "<SEMANTIC_API_BASE_URL>/health" 60
$trackReady = Wait-Health "<TOKEN_TRACKING_BASE_URL>/health" 60

Write-Host ""
Write-Host "=== Semantic Workspace Status ==="
Write-Host "Primary API (test_repo) ready : $apiReady"
Write-Host "Token tracking dashboard ready: $trackReady"
Write-Host ""
Write-Host "=== URLs ==="
Write-Host "  Semantic UI          : <SEMANTIC_API_BASE_URL>/semantic_ui"
Write-Host "  Token Tracking       : <TOKEN_TRACKING_BASE_URL>"
Write-Host "  API health           : <SEMANTIC_API_BASE_URL>/health"
Write-Host "  MCP tools            : <MCP_BRIDGE_BASE_URL>/mcp/tools"
Write-Host ""
Write-Host "=== Multi-repo note ==="
Write-Host "  Each project needs its own API instance on a separate port."
Write-Host "  The semantic API is single-repo per process by design."
Write-Host "  Token tracking reads from test_repo/.semantic/token_tracking/"
Write-Host ""
Write-Host "=== MCP token ==="
Write-Host "  x-mcp-token: $($env:MCP_BRIDGE_TOKEN)"
