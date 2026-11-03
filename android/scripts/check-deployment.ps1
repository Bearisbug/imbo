<#
.SYNOPSIS
  检查一个待发布的部署：imbo 服务端、ntfy、以及（可选）发一条测试通知。

.DESCRIPTION
  正式出包前先跑这个。它只做只读请求，外加一次（可关掉的）ntfy 发布，
  用来确认「手机能连到的地址」真的活着，且 ntfy 能收能发。

.EXAMPLE
  ./scripts/check-deployment.ps1 -BaseUrl https://imbo.abc.ts.net -NtfyUrl https://ntfy.abc.ts.net -Topic imbo-xxxxxxxxxxxx
#>
param(
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [string]$NtfyUrl = "",
    [string]$Topic = "",
    [switch]$SkipTestNotification
)

$ErrorActionPreference = "Stop"
$failures = New-Object System.Collections.Generic.List[string]

function Test-Url([string]$url) {
    try {
        $res = Invoke-WebRequest -Uri $url -TimeoutSec 10 -UseBasicParsing
        return $res.Content
    } catch {
        return $null
    }
}

$base = $BaseUrl.TrimEnd('/')
$isLocal = $base -match '^https?://(127\.0\.0\.1|localhost|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|100\.)'
if ($base.StartsWith("http://") -and -not $isLocal) {
    $failures.Add("服务端用的是明文 http，且不是本机/局域网/尾网地址：release 包会拒绝连接。改用 https。")
}

Write-Host "1) imbo 服务端 $base/v1/health"
$health = Test-Url "$base/v1/health"
if (-not $health) {
    $failures.Add("imbo 服务端没有响应：$base")
} else {
    try {
        $json = $health | ConvertFrom-Json
        if ($json.status -ne "ok") { $failures.Add("imbo 服务端状态不是 ok：$health") }
        else {
            $push = $json.push
            $channels = if ($push) { "expo=$($push.expo) fcm=$($push.fcm) ntfy=$($push.ntfy)" } else { "（该版本不报推送通道）" }
            Write-Host "   ok，版本 $($json.version)，db=$($json.db)，推送 $channels" -ForegroundColor Green
            if ($NtfyUrl -and $push -and -not $push.ntfy) {
                $failures.Add("服务端没配 ntfy（IMBO_NTFY_URL），手机不会收到推送")
            }
        }
    } catch {
        $failures.Add("imbo 服务端返回的不是 JSON：$health")
    }
}

if ($NtfyUrl) {
    $ntfy = $NtfyUrl.TrimEnd('/')
    Write-Host "2) ntfy $ntfy/v1/health"
    $nh = Test-Url "$ntfy/v1/health"
    if (-not $nh) {
        $failures.Add("ntfy 没有响应：$ntfy")
    } else {
        try {
            $nj = $nh | ConvertFrom-Json
            if (-not $nj.healthy) { $failures.Add("ntfy 不健康：$nh") }
            else { Write-Host "   ok，role=$($nj.role)" -ForegroundColor Green }
        } catch {
            $failures.Add("ntfy 返回的不是 JSON：$nh")
        }
    }

    if ($Topic -and -not $SkipTestNotification) {
        Write-Host "3) 往话题 $Topic 发一条测试通知"
        $body = @{
            topic   = $Topic
            title   = "imbo 部署自检"
            message = "如果你在手机上看到这条，说明 ntfy 链路是通的"
            priority = 3
            tags    = @("white_check_mark")
        } | ConvertTo-Json -Compress
        try {
            # 中文要显式 UTF-8 字节，不然 PowerShell 按本地代码页发出去，ntfy 那边就是乱码
            $bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
            $r = Invoke-WebRequest -Uri $ntfy -Method Post -Body $bytes -ContentType "application/json; charset=utf-8" -TimeoutSec 10 -UseBasicParsing
            if ($r.StatusCode -eq 200) { Write-Host "   ok，已发布" -ForegroundColor Green }
            else { $failures.Add("ntfy 发布失败：HTTP $($r.StatusCode)") }
        } catch {
            $failures.Add("ntfy 发布失败：$($_.Exception.Message)")
        }
    }
}

""
if ($failures.Count -gt 0) {
    Write-Host "检查未通过：" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
Write-Host "全部通过。" -ForegroundColor Green
