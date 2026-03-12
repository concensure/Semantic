param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("health", "list-tools", "call-tool", "retrieve", "ab-test-dev", "ab-test-dev-results", "llm-tools")]
  [string]$Command,
  [string]$Tool,
  [string]$InputJson = "{}",
  [string]$BaseUrl = "<SEMANTIC_API_BASE_URL>",
  [string]$BridgeUrl = "<MCP_BRIDGE_BASE_URL>",
  [string]$Token = "<SET_LOCAL_VALUE>"
)

$ErrorActionPreference = "Stop"

switch ($Command) {
  "health" {
    Invoke-RestMethod -Method Get -Uri "$BaseUrl/health" | ConvertTo-Json -Depth 8
  }
  "llm-tools" {
    Invoke-RestMethod -Method Get -Uri "$BaseUrl/llm_tools" | ConvertTo-Json -Depth 8
  }
  "retrieve" {
    Invoke-RestMethod -Method Post -Uri "$BaseUrl/retrieve" -ContentType "application/json" -Body $InputJson | ConvertTo-Json -Depth 8
  }
  "ab-test-dev" {
    Invoke-RestMethod -Method Post -Uri "$BaseUrl/ab_test_dev" -ContentType "application/json" -Body $InputJson | ConvertTo-Json -Depth 8
  }
  "ab-test-dev-results" {
    Invoke-RestMethod -Method Get -Uri "$BaseUrl/ab_test_dev" | ConvertTo-Json -Depth 8
  }
  "list-tools" {
    Invoke-RestMethod -Method Get -Uri "$BridgeUrl/mcp/tools" | ConvertTo-Json -Depth 8
  }
  "call-tool" {
    if (-not $Tool) {
      throw "Tool is required when Command=call-tool"
    }
    $body = @{
      tool = $Tool
      input = ($InputJson | ConvertFrom-Json)
    } | ConvertTo-Json -Depth 12
    Invoke-RestMethod -Method Post -Uri "$BridgeUrl/mcp/tools/call" -Headers @{ "x-mcp-token" = $Token } -ContentType "application/json" -Body $body | ConvertTo-Json -Depth 12
  }
}
