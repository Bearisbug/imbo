# Runs the JVM unit tests.
#
# Why the junction: with JDK 17 on a Chinese Windows install, the test worker
# resolves its classpath through the ANSI code page, so a project living under a
# path with non-ASCII characters ends up with "ClassNotFoundException: <own test
# class>". A directory junction under C:\Temp gives Gradle an ASCII path to work
# with; copying the tree with robocopy used to hang on locked build outputs.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\run-tests.ps1
$ErrorActionPreference = "Stop"

$moduleRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$isAscii = ($moduleRoot -notmatch '[^\x00-\x7F]')

$env:JAVA_HOME = if ($env:JAVA_HOME) { $env:JAVA_HOME } else { "C:\DevTools\jdk-17" }
$env:ANDROID_HOME = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "C:\DevTools\android-sdk" }
$env:GRADLE_USER_HOME = if ($env:GRADLE_USER_HOME) { $env:GRADLE_USER_HOME } else { "C:\DevTools\gradle-home" }

if ($isAscii) {
    Write-Output "running in place: $moduleRoot"
    Set-Location $moduleRoot
    & "$moduleRoot\gradlew.bat" :app:testDebugUnitTest --console=plain
    exit $LASTEXITCODE
}

$linkRoot = if (Test-Path "C:\Temp") { "C:\Temp\imbo-android-src" } else { "C:\imbo-android-src" }
if (Test-Path $linkRoot) { cmd /c "rmdir `"$linkRoot`"" | Out-Null }
cmd /c "mklink /J `"$linkRoot`" `"$moduleRoot`"" | Out-Null
Write-Output "project path has non-ASCII characters, running through $linkRoot"

Set-Location $linkRoot
& "$linkRoot\gradlew.bat" :app:testDebugUnitTest --console=plain
exit $LASTEXITCODE
