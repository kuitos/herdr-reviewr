# First-logon setup for the reviewr Windows QA seat. Runs once as the auto-logon user
# `reviewr` (an administrator). Leaves C:\setup.log, and C:\setup-done.txt on success.
param([string]$Media = "E:")
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Start-Transcript -Path C:\setup.log -Append
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Step($name, [scriptblock]$body) {
    Write-Host "== $name"
    try { & $body } catch { Write-Host "!! $name failed: $_"; throw }
}

Step "power: never sleep, never blank" {
    powercfg -H OFF
    powercfg /change standby-timeout-ac 0
    powercfg /change monitor-timeout-ac 0
    reg add "HKCU\Control Panel\Desktop" /v ScreenSaveActive /t REG_SZ /d 0 /f | Out-Null
}

Step "virtio drivers and UTM guest tools" {
    Get-ChildItem "$Media\Drivers" -Recurse -Filter *.inf |
        Where-Object { $_.FullName -match '\\w11\\ARM64\\' } |
        ForEach-Object { pnputil /add-driver $_.FullName /install | Out-Null }
    $tools = Get-ChildItem "$Media\utm-guest-tools-*.exe" | Select-Object -First 1
    Start-Process $tools.FullName -ArgumentList "/S" -Wait
}

Step "wait for the network" {
    $deadline = (Get-Date).AddMinutes(10)
    while (-not (Test-NetConnection github.com -Port 443 -InformationLevel Quiet)) {
        if ((Get-Date) -gt $deadline) { throw "no network after 10 minutes" }
        Start-Sleep -Seconds 5
    }
}

Step "OpenSSH server with the Mac's key, PowerShell as its shell" {
    Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
    Set-Service -Name sshd -StartupType Automatic
    Start-Service sshd
    New-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -PropertyType String -Force `
        -Value "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" | Out-Null
    $keys = "C:\ProgramData\ssh\administrators_authorized_keys"
    Copy-Item "$Media\authorized_keys" $keys -Force
    icacls $keys /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F" | Out-Null
    if (-not (Get-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -DisplayName "OpenSSH Server (sshd)" `
            -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 | Out-Null
    }
    # QEMU's user network lands in the Public profile; the capability's rule covers Private only.
    Set-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -Profile Any
}

Step "Git for Windows (installer defaults)" {
    $rel = Invoke-RestMethod "https://api.github.com/repos/git-for-windows/git/releases/latest"
    $asset = $rel.assets | Where-Object { $_.name -match '^Git-[\d.]+(\.\d+)?-arm64\.exe$' } | Select-Object -First 1
    if (-not $asset) { $asset = $rel.assets | Where-Object { $_.name -match '^Git-[\d.]+(\.\d+)?-64-bit\.exe$' } | Select-Object -First 1 }
    $exe = "$env:TEMP\$($asset.name)"
    Invoke-WebRequest $asset.browser_download_url -OutFile $exe
    Start-Process $exe -ArgumentList "/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES" -Wait
}

Step "herdr" {
    # Its own process: the installer ends with `exit`, which would end this script too.
    powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://herdr.dev/install.ps1 | iex"
}

"done $(Get-Date -Format o)" | Set-Content C:\setup-done.txt
Stop-Transcript
