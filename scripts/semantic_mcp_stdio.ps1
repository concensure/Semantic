$ErrorActionPreference = "Stop"

$baseUrl = if ($env:SEMANTIC_BASE_URL) { $env:SEMANTIC_BASE_URL } else { "<SEMANTIC_API_BASE_URL>" }
$logPath = $env:SEMANTIC_MCP_LOG

function Write-Log {
    param([string]$Line)
    if ([string]::IsNullOrWhiteSpace($logPath)) {
        return
    }
    Add-Content -Path $logPath -Value $Line
}

function Write-JsonResponse {
    param(
        [Parameter(Mandatory = $true)] $Payload,
        [Parameter(Mandatory = $true)][string]$Transport
    )

    $json = $Payload | ConvertTo-Json -Depth 32 -Compress
    if ($Transport -eq "jsonl") {
        [Console]::Out.WriteLine($json)
    } else {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
        [Console]::OpenStandardOutput().Write(
            [System.Text.Encoding]::ASCII.GetBytes("Content-Length: $($bytes.Length)`r`n`r`n"),
            0,
            [System.Text.Encoding]::ASCII.GetByteCount("Content-Length: $($bytes.Length)`r`n`r`n")
        )
        [Console]::OpenStandardOutput().Write($bytes, 0, $bytes.Length)
        [Console]::OpenStandardOutput().Flush()
    }
}

function Read-ContentLengthMessage {
    param($Reader, [string]$FirstLine)

    $contentLength = $null
    if ($FirstLine -match '^\s*Content-Length\s*:\s*(\d+)\s*$') {
        $contentLength = [int]$matches[1]
    }

    while ($true) {
        $line = $Reader.ReadLine()
        if ($null -eq $line) {
            return $null
        }
        if ($line -eq "") {
            break
        }
        if ($line -match '^\s*Content-Length\s*:\s*(\d+)\s*$') {
            $contentLength = [int]$matches[1]
        }
    }

    if ($null -eq $contentLength) {
        Write-Log "unexpected-preamble $FirstLine"
        return "__skip__"
    }

    $chars = New-Object char[] $contentLength
    $read = 0
    while ($read -lt $contentLength) {
        $count = $Reader.Read($chars, $read, $contentLength - $read)
        if ($count -le 0) {
            return $null
        }
        $read += $count
    }

    return -join $chars
}

function Read-NextMessage {
    param($Reader)

    while ($true) {
        $line = $Reader.ReadLine()
        if ($null -eq $line) {
            return $null
        }
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line.TrimStart().StartsWith("{")) {
            return @{
                transport = "jsonl"
                raw = $line
            }
        }

        $raw = Read-ContentLengthMessage -Reader $Reader -FirstLine $line
        if ($null -eq $raw) {
            return $null
        }
        if ($raw -eq "__skip__") {
            continue
        }
        return @{
            transport = "content-length"
            raw = $raw
        }
    }
}

function Invoke-SemanticTool {
    param(
        [Parameter(Mandatory = $true)][string]$ToolName,
        [Parameter(Mandatory = $true)]$Arguments
    )

    switch ($ToolName) {
        "retrieve" { $endpoint = "/retrieve" }
        "ide_autoroute" { $endpoint = "/ide_autoroute" }
        default { throw "unsupported tool '$ToolName'" }
    }

    $body = if ($null -eq $Arguments) { @{} } else { $Arguments }
    $jsonBody = $body | ConvertTo-Json -Depth 32 -Compress
    return Invoke-RestMethod -Method Post -Uri ($baseUrl + $endpoint) -ContentType "application/json" -Body $jsonBody
}

Write-Log "startup"

$stdin = [Console]::OpenStandardInput()
$reader = New-Object System.IO.StreamReader($stdin, [System.Text.Encoding]::UTF8)
$transport = "content-length"

while ($true) {
    $packet = Read-NextMessage -Reader $reader
    if ($null -eq $packet) {
        break
    }

    $transport = $packet.transport
    try {
        $message = $packet.raw | ConvertFrom-Json
    } catch {
        Write-Log "malformed-input $($_.Exception.Message)"
        continue
    }

    $id = $message.id
    $method = [string]$message.method
    Write-Log "recv method=$method id=$id"

    switch ($method) {
        "initialize" {
            $requestedVersion = $message.params.protocolVersion
            if ([string]::IsNullOrWhiteSpace($requestedVersion)) {
                $requestedVersion = "2024-11-05"
            }
            $response = @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    protocolVersion = $requestedVersion
                    capabilities = @{
                        tools = @{ listChanged = $false }
                        prompts = @{ listChanged = $false }
                        resources = @{ subscribe = $false; listChanged = $false }
                    }
                    serverInfo = @{
                        name = "semantic"
                        version = "0.1.0"
                    }
                }
            }
            Write-JsonResponse -Payload $response -Transport $transport
            continue
        }
        "notifications/initialized" { continue }
        "$/cancelRequest" { continue }
        "notifications/cancelled" { continue }
        "ping" {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{}
            } -Transport $transport
            continue
        }
        "tools/list" {
            $tools = @(
                @{
                    name = "retrieve"
                    description = "Unified retrieval tool."
                    inputSchema = @{
                        type = "object"
                        additionalProperties = $true
                    }
                },
                @{
                    name = "ide_autoroute"
                    description = "Semantic-first IDE entrypoint."
                    inputSchema = @{
                        type = "object"
                        additionalProperties = $true
                    }
                }
            )
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    tools = $tools
                }
            } -Transport $transport
            continue
        }
        "prompts/list" {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    prompts = @()
                }
            } -Transport $transport
            continue
        }
        "resources/list" {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    resources = @()
                }
            } -Transport $transport
            continue
        }
        "resources/templates/list" {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    resourceTemplates = @()
                }
            } -Transport $transport
            continue
        }
        "completion/complete" {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                result = @{
                    completion = @{
                        values = @()
                        total = 0
                        hasMore = $false
                    }
                }
            } -Transport $transport
            continue
        }
        "tools/call" {
            $toolName = [string]$message.params.name
            Write-Log "tools/call name=$toolName"
            try {
                $result = Invoke-SemanticTool -ToolName $toolName -Arguments $message.params.arguments
                $resultJson = $result | ConvertTo-Json -Depth 32 -Compress
                Write-JsonResponse -Payload @{
                    jsonrpc = "2.0"
                    id = $id
                    result = @{
                        content = @(
                            @{
                                type = "text"
                                text = $resultJson
                            }
                        )
                        structuredContent = $result
                    }
                } -Transport $transport
            } catch {
                $errorMessage = $_.Exception.Message
                Write-Log "tools/call error=$errorMessage"
                Write-JsonResponse -Payload @{
                    jsonrpc = "2.0"
                    id = $id
                    error = @{
                        code = -32000
                        message = $errorMessage
                    }
                } -Transport $transport
            }
            continue
        }
        default {
            Write-JsonResponse -Payload @{
                jsonrpc = "2.0"
                id = $id
                error = @{
                    code = -32601
                    message = "unsupported method '$method'"
                }
            } -Transport $transport
            continue
        }
    }
}
