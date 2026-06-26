# naTiVeFS Windows installer
# Run in PowerShell as normal user

$GITHUB_REPO = "TomasekValla/naTiVeFS"
$BIN_URL = "https://github.com/$GITHUB_REPO/releases/latest/download/nativefs-windows-x64.exe"
$INSTALL_DIR = "$env:LOCALAPPDATA\naTiVeFS"
$BINARY = "$INSTALL_DIR\nativefs.exe"

Write-Host "Installing naTiVeFS..." -ForegroundColor White

New-Item -ItemType Directory -Force -Path $INSTALL_DIR | Out-Null

Write-Host "Downloading binary..."
Invoke-WebRequest -Uri $BIN_URL -OutFile $BINARY

# Add to PATH for current user
$UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -notlike "*$INSTALL_DIR*") {
    [System.Environment]::SetEnvironmentVariable("PATH", "$UserPath;$INSTALL_DIR", "User")
}

# Add "Send To" shortcut
$SendTo = [System.IO.Path]::Combine($env:APPDATA, "Microsoft\Windows\SendTo")
$WshShell = New-Object -comObject WScript.Shell
$Shortcut = $WshShell.CreateShortcut("$SendTo\naTiVeFS.lnk")
$Shortcut.TargetPath = $BINARY
$Shortcut.Description = "Upload with naTiVeFS"
$Shortcut.Save()

Write-Host ""
Write-Host "naTiVeFS installed to $BINARY" -ForegroundColor Green
Write-Host "Right-click any file -> Send to -> naTiVeFS"
Write-Host ""
Write-Host "Run: nativefs"
