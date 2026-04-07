param(
  [string]$OutputPath = "docs/doc_ignore/quality_report.json",
  [string]$BaselinePath = "docs/doc_ignore/quality_report_baseline.json",
  [string]$SummaryPath = "docs/doc_ignore/quality_report_summary.md",
  [string]$HistoryPath = "docs/doc_ignore/quality_report_history.json",
  [string]$SnapshotPath = "docs/doc_ignore/quality_report_trend_snapshot.json",
  [switch]$WriteBaseline,
  [switch]$SkipWarmup,
  [int]$MeasuredPasses = 3
)

$ErrorActionPreference = "Stop"

$args = @($OutputPath, "--baseline", $BaselinePath, "--summary", $SummaryPath, "--history", $HistoryPath, "--snapshot", $SnapshotPath)
if ($WriteBaseline) {
  $args += "--write-baseline"
}

if (-not $SkipWarmup) {
  $previousErrorActionPreference = $ErrorActionPreference
  try {
    $ErrorActionPreference = "Continue"
    cargo run -p test_support --example export_quality_report -- @args | Out-Null
  } catch {
    Write-Host "warmup export failed; continuing to measured pass" -ForegroundColor DarkYellow
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
}

if ($MeasuredPasses -lt 1) {
  throw "MeasuredPasses must be at least 1."
}

$lastErrorRecord = $null
for ($attempt = 1; $attempt -le $MeasuredPasses; $attempt++) {
  try {
    cargo run -p test_support --example export_quality_report -- @args
    $lastErrorRecord = $null
    break
  } catch {
    $lastErrorRecord = $_
    if ($attempt -lt $MeasuredPasses) {
      Write-Host "measured export attempt $attempt failed; retrying..." -ForegroundColor DarkYellow
    }
  }
}

if ($null -ne $lastErrorRecord) {
  throw $lastErrorRecord
}
