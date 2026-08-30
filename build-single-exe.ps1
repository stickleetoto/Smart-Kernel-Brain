$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

function Assert-RequiredFile([string]$RelativePath) {
    $FullPath = Join-Path $Root $RelativePath
    if (-not (Test-Path -LiteralPath $FullPath -PathType Leaf)) {
        throw "Required source file is missing: $RelativePath"
    }
    Write-Host "OK      $RelativePath"
}

Write-Host ""
Write-Host "== Single EXE source layout check =="
Assert-RequiredFile "src\bin\skb.rs"
Assert-RequiredFile "src\bin\install.rs"
Assert-RequiredFile "src\bin\mcp.rs"

function Run-Step([string]$Name, [scriptblock]$Block) {
    Write-Host ""
    Write-Host "== $Name =="
    & $Block
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

Run-Step "Core freeze check" { python .\scripts\verify_core_freeze.py }
Run-Step "Rust tests" { cargo test }
Run-Step "Release build" { cargo build --release --bin skb }

$Dist = Join-Path $Root "dist"
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
$SourceExe = Join-Path $Root "target\release\skb.exe"
$FinalExe = Join-Path $Dist "SKB.exe"
Copy-Item -LiteralPath $SourceExe -Destination $FinalExe -Force

Run-Step "Single EXE smoke check" { & $FinalExe --version }

$Hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $FinalExe).Hash.ToLowerInvariant()
$HashLine = "$Hash  SKB.exe"
Set-Content -LiteralPath (Join-Path $Dist "SKB.exe.sha256.txt") -Value $HashLine -Encoding ascii

Write-Host ""
Write-Host "[OK] Single-binary build complete"
Write-Host "EXE    : $FinalExe"
Write-Host "SHA256 : $Hash"
Write-Host ""
Write-Host "Double-click dist\SKB.exe to test first-run installation."
