<#
.SYNOPSIS
  构建可交付的 release 包（APK + AAB），并打印校验和。

.DESCRIPTION
  服务端地址和 ntfy 地址都进 BuildConfig；不给 ntfy 地址时账号页只显示话题、
  不显示订阅链接。签名用 android/keystore.properties（不入库）。

.EXAMPLE
  ./scripts/release.ps1 -BaseUrl https://imbo.abc.ts.net -NtfyUrl https://ntfy.abc.ts.net
#>
param(
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [string]$NtfyUrl = "",
    [string]$VersionName = ""
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path "keystore.properties")) {
    throw "缺少 keystore.properties：release 包必须签名，先把 storeFile/口令/别名填好。"
}

$env:JAVA_HOME = if ($env:JAVA_HOME) { $env:JAVA_HOME } else { "C:\DevTools\jdk-17" }
$env:ANDROID_HOME = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "C:\DevTools\android-sdk" }

$gradleArgs = @(
    ":app:assembleRelease",
    ":app:bundleRelease",
    "-Pimbo.useMock=false",
    "-Pimbo.baseUrl=$BaseUrl",
    "-Pimbo.ntfyUrl=$NtfyUrl",
    "--console=plain"
)

if ($VersionName) {
    # versionName 在 gradle 里写死，这里只做提醒，避免两份真相
    Write-Host "提示：versionName 目前由 app/build.gradle.kts 决定，-VersionName 只用于核对。" -ForegroundColor Yellow
    $declared = (Select-String -Path "app/build.gradle.kts" -Pattern 'versionName = "([^"]+)"').Matches[0].Groups[1].Value
    if ($declared -ne $VersionName) {
        throw "版本不一致：build.gradle.kts 是 $declared，参数是 $VersionName"
    }
}

& .\gradlew.bat @gradleArgs
if ($LASTEXITCODE -ne 0) { throw "构建失败，退出码 $LASTEXITCODE" }

$apk = "app/build/outputs/apk/release/app-release.apk"
$aab = "app/build/outputs/bundle/release/app-release.aab"
foreach ($f in @($apk, $aab)) {
    if (-not (Test-Path $f)) { throw "没有产物：$f" }
    $hash = (Get-FileHash $f -Algorithm SHA256).Hash.ToLower()
    $size = [math]::Round((Get-Item $f).Length / 1MB, 2)
    Write-Host ("{0}  {1} MB  sha256={2}" -f $f, $size, $hash)
}

$dist = Join-Path (Resolve-Path "..\..\dist") ""
Copy-Item $apk (Join-Path $dist "imbo-release-signed.apk") -Force
Copy-Item $aab (Join-Path $dist "imbo-release.aab") -Force
Write-Host "已复制到 $dist（imbo-release-signed.apk / imbo-release.aab）"
