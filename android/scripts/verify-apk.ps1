<#
.SYNOPSIS
  校验一个 release APK：签名证书、版本号、是否内置了正确的服务端地址。

.DESCRIPTION
  交给别人之前跑一次，确认发出去的包是「自己的 keystore 签的、版本对、指向对的服务器」。
  申请发布包的人也可以用它核对下载到的 APK。

.EXAMPLE
  ./scripts/verify-apk.ps1 ../dist/imbo-release-signed.apk -BaseUrl https://imbo.example.com
#>
param(
    [Parameter(Mandatory = $true)][string]$Apk,
    [string]$BaseUrl = "",
    [string]$NtfyUrl = ""
)

$ErrorActionPreference = "Stop"
if (-not (Test-Path $Apk)) { throw "找不到 APK：$Apk" }

$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "$env:LOCALAPPDATA\Android\Sdk" }
$buildTools = Get-ChildItem "$sdk\build-tools" -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $buildTools) { throw "找不到 build-tools（ANDROID_HOME=$sdk）" }

$apksigner = Join-Path $buildTools.FullName "apksigner.bat"
$aapt2 = Join-Path $buildTools.FullName "aapt2.exe"

$size = [math]::Round((Get-Item $Apk).Length / 1MB, 2)
$sha = (Get-FileHash $Apk -Algorithm SHA256).Hash.ToLower()
Write-Host "APK      $Apk"
Write-Host "大小     $size MB"
Write-Host "sha256   $sha"

Write-Host ""
Write-Host "签名证书："
& $apksigner verify --print-certs $Apk | Select-String 'SHA-256 digest|DN:' | ForEach-Object { "  $($_.Line.Trim())" }

if (Test-Path $aapt2) {
    Write-Host ""
    Write-Host "包信息："
    & $aapt2 dump badging $Apk 2>$null | Select-String "^package:|^application-label:" |
        ForEach-Object { "  $($_.Line.Trim())" }
}

if ($BaseUrl -or $NtfyUrl) {
    Write-Host ""
    Write-Host "内置地址检查："
    $tmp = Join-Path $env:TEMP ("apkcheck-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        Copy-Item $Apk (Join-Path $tmp "a.zip")
        Expand-Archive (Join-Path $tmp "a.zip") -DestinationPath (Join-Path $tmp "x") -Force
        $dex = ""
        Get-ChildItem (Join-Path $tmp "x") -Filter *.dex | ForEach-Object {
            $dex += [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($_.FullName))
        }
        if ($BaseUrl) {
            Write-Host ("  baseUrl {0}: {1}" -f $BaseUrl, ($dex.Contains($BaseUrl)))
        }
        if ($NtfyUrl) {
            Write-Host ("  ntfyUrl {0}: {1}" -f $NtfyUrl, ($dex.Contains($NtfyUrl)))
        }
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ""
Write-Host "完成。" -ForegroundColor Green
