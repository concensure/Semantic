param(
  [string]$RepoPath = "test_repo",
  [string]$SemanticBaseUrl = "<SEMANTIC_API_BASE_URL>",
  [string]$ApiHealthUrl = "<SEMANTIC_API_BASE_URL>/health",
  [string]$McpHealthUrl = "<MCP_BRIDGE_BASE_URL>/health",
  [switch]$StopExisting
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$tmp = Join-Path $root ".tmp"
New-Item -ItemType Directory -Force $tmp | Out-Null

if ($StopExisting) {
  Get-Process api,mcp_bridge,cargo -ErrorAction SilentlyContinue | Stop-Process -Force
}

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
$env:SEMANTIC_BASE_URL = $SemanticBaseUrl

$apiOut = Join-Path $tmp "api.out.log"
$apiErr = Join-Path $tmp "api.err.log"
$mcpOut = Join-Path $tmp "mcp.out.log"
$mcpErr = Join-Path $tmp "mcp.err.log"

Start-Process -FilePath cargo -ArgumentList "run -p api -- $RepoPath" -WorkingDirectory $root -RedirectStandardOutput $apiOut -RedirectStandardError $apiErr -PassThru | Out-Null
Start-Process -FilePath mcp_bridge -WorkingDirectory $root -RedirectStandardOutput $mcpOut -RedirectStandardError $mcpErr -PassThru | Out-Null

function Wait-Health($url, $seconds) {
  $ok = $false
  for ($i = 0; $i -lt ($seconds * 2); $i++) {
    Start-Sleep -Milliseconds 500
    try {
      $r = Invoke-RestMethod -Method Get -Uri $url -TimeoutSec 1
      if ($r.status -eq "ok") { $ok = $true; break }
    } catch {}
  }
  return $ok
}

$apiReady = Wait-Health $ApiHealthUrl 30
$mcpReady = Wait-Health $McpHealthUrl 30

Write-Host ""
Write-Host "Semantic start status"
Write-Host "API ready: $apiReady"
Write-Host "MCP ready: $mcpReady"
Write-Host ""
Write-Host "URLs"
Write-Host "Semantic UI: <SEMANTIC_API_BASE_URL>/semantic_ui"
Write-Host "API health : $ApiHealthUrl"
Write-Host "MCP root   : <MCP_BRIDGE_BASE_URL>/"
Write-Host "MCP tools  : <MCP_BRIDGE_BASE_URL>/mcp/tools"
Write-Host ""
Write-Host "Bridge token source"
Write-Host "MCP_BRIDGE_TOKEN env is set in this shell session."
Write-Host "Token file: $tokenFile"
Write-Host ""
Write-Host "Example MCP call header"
Write-Host "x-mcp-token: $($env:MCP_BRIDGE_TOKEN)"
