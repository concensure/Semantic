param(
  [string]$InstallRoot = "",
  [switch]$Force
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
  if ($env:CARGO_HOME -and -not [string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    $InstallRoot = $env:CARGO_HOME
  } else {
    $InstallRoot = Join-Path $HOME ".cargo"
  }
}

$installArgs = @(
  "install",
  "--path", "semantic_cli",
  "--locked",
  "--root", $InstallRoot,
  "--bin", "semantic"
)

if ($Force) {
  $installArgs += "--force"
}

Write-Host "Installing Semantic CLI to $InstallRoot"
Push-Location $repoRoot
try {
  cargo @installArgs
  $binDir = Join-Path $InstallRoot "bin"
  $exeName = if ($IsWindows) { "semantic.exe" } else { "semantic" }
  $binaryPath = Join-Path $binDir $exeName
  Write-Host ""
  Write-Host "Installed binary:"
  Write-Host "  $binaryPath"
  Write-Host ""
  Write-Host "Try it on another repo:"
  Write-Host "  semantic --repo C:\path\to\project status"
  Write-Host "  semantic --repo C:\path\to\project route --task `"explain auth flow`""
} finally {
  Pop-Location
}
