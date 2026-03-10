param(
    [string]$FactoryRoot = "D:\ng-gateway-factory",
    [string]$ImageVersion = "v1.0.0",
    [string]$LoaderFile = "rk3588_spl_loader_v1.15.113.bin"
)

$ErrorActionPreference = "Stop"

function Write-Info {
    param([string]$Message)
    Write-Host "[INFO] $Message" -ForegroundColor Cyan
}

function Write-Pass {
    param([string]$Message)
    Write-Host "[PASS] $Message" -ForegroundColor Green
}

function Write-Fail {
    param([string]$Message)
    Write-Host "[FAIL] $Message" -ForegroundColor Red
}

function Assert-Exists {
    param(
        [string]$Path,
        [string]$Label
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$Label not found: $Path"
    }

    Write-Pass "$Label found: $Path"
}

function Read-ExpectedSha256 {
    param([string]$Sha256File)

    $content = Get-Content -LiteralPath $Sha256File -ErrorAction Stop | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($content)) {
        throw "SHA256 file is empty: $Sha256File"
    }

    return ($content -split '\s+')[0].Trim().ToLowerInvariant()
}

try {
    $imageDir = Join-Path $FactoryRoot ("images\" + "ng-gateway-" + $ImageVersion)
    $loaderPath = Join-Path $FactoryRoot ("loader\" + $LoaderFile)
    $rawImagePath = Join-Path $imageDir ("ng-gateway-" + $ImageVersion + ".img")
    $rawSha256Path = Join-Path $imageDir ("ng-gateway-" + $ImageVersion + ".img.sha256")
    $manifestPath = Join-Path $imageDir ("ng-gateway-" + $ImageVersion + ".manifest.json")

    Write-Info "NG Gateway flash preflight"
    Write-Info "Factory root: $FactoryRoot"
    Write-Info "Image version: $ImageVersion"
    Write-Host ""

    Assert-Exists -Path $loaderPath -Label "RK3588 loader"
    Assert-Exists -Path $rawImagePath -Label "Raw image"
    Assert-Exists -Path $rawSha256Path -Label "Raw image SHA256"
    Assert-Exists -Path $manifestPath -Label "Manifest"

    $rawImageFile = Get-Item -LiteralPath $rawImagePath
    if ($rawImageFile.Length -le 0) {
        throw "Raw image is empty: $rawImagePath"
    }
    Write-Pass ("Raw image size: {0:N0} bytes" -f $rawImageFile.Length)

    Write-Info "Verifying raw image SHA256..."
    $expectedSha256 = Read-ExpectedSha256 -Sha256File $rawSha256Path
    $actualSha256 = (Get-FileHash -LiteralPath $rawImagePath -Algorithm SHA256).Hash.ToLowerInvariant()

    if ($expectedSha256 -ne $actualSha256) {
        throw "SHA256 mismatch. Expected=$expectedSha256 Actual=$actualSha256"
    }
    Write-Pass "Raw image SHA256 verified"

    Write-Info "Validating manifest..."
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

    if ($manifest.version -ne $ImageVersion) {
        throw "Manifest version mismatch. Expected=$ImageVersion Actual=$($manifest.version)"
    }

    if ($manifest.sha256_raw -and ($manifest.sha256_raw.ToLowerInvariant() -ne $actualSha256)) {
        throw "Manifest sha256_raw mismatch. Manifest=$($manifest.sha256_raw) Actual=$actualSha256"
    }

    Write-Pass "Manifest version and sha256_raw verified"
    Write-Host ""
    Write-Host "----------------------------------------" -ForegroundColor DarkGray
    Write-Host "Factory image preflight passed." -ForegroundColor Green
    Write-Host "RKDevTool can now be opened and pointed to:" -ForegroundColor Green
    Write-Host "  Loader: $loaderPath"
    Write-Host "  Image : $rawImagePath"
    Write-Host "----------------------------------------" -ForegroundColor DarkGray
    exit 0
}
catch {
    Write-Host ""
    Write-Fail $_.Exception.Message
    Write-Host "Do NOT flash until the preflight check passes." -ForegroundColor Yellow
    exit 1
}
