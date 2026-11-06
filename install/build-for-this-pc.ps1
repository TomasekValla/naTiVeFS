# Builds naTiVeFS tuned for this very computer into %LOCALAPPDATA%\Programs\naTiVeFS-tuned and removes everything else.
# Needs Rust (https://rustup.rs) with the Visual Studio C++ build tools, CMake, Git (with Git Bash) and 7-Zip.
# Most people should use install/windows.ps1 instead: a tuned build takes a while and does not update itself.
#   irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/build-for-this-pc.ps1 | iex
$ErrorActionPreference = "Stop"
foreach ($tool in "cargo", "git", "cmake") {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "Install $tool first, see INSTALLATION.md" }
}
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "nativefs-build-$PID"
$dest = Join-Path $env:LOCALAPPDATA "Programs\naTiVeFS-tuned"
try {
    git clone --quiet --depth 1 "https://github.com/TomasekValla/naTiVeFS.git" $tmp
    Set-Location $tmp
    & "$env:ProgramFiles\Git\bin\bash.exe" tools/fetch-libmpv.sh $arch
    if ($LASTEXITCODE) { throw "libmpv could not be downloaded" }
    $env:RUSTFLAGS = "-C target-cpu=native"
    $env:CARGO_PROFILE_RELEASE_LTO = "fat"
    cargo build --release --locked -p nativefs-app -p nativefs-link
    if ($LASTEXITCODE) { throw "The build failed" }
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item target\release\naTiVeFS.exe, target\release\naTiVeFS-link.exe, target\release\libmpv-2.dll, LICENSE, packaging\THIRD-PARTY.txt $dest
    Write-Host "Done: $dest\naTiVeFS.exe"
} finally {
    Set-Location $env:TEMP
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
