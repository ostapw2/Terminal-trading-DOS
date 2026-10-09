# install.ps1 - installer for dos-commander on Windows (PowerShell 5+).
#
#   irm https://raw.githubusercontent.com/ostapw2/Terminal-trading-DOS/main/install.ps1 | iex
#
# Downloads the latest release zip, VERIFIES its SHA-256, installs to
# %LOCALAPPDATA%\dos and adds that folder to your user PATH. No admin needed.
# Environment: DOS_TAG (default: latest), DOS_INSTALL_DIR, DOS_REPO.
# While the repository is private the download needs the GitHub CLI
# (`gh auth login` once); this script then uses `gh release download`.

$ErrorActionPreference = 'Stop'

$repo = if ($env:DOS_REPO) { $env:DOS_REPO } else { 'ostapw2/Terminal-trading-DOS' }
$dir  = if ($env:DOS_INSTALL_DIR) { $env:DOS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'dos' }
$tag  = $env:DOS_TAG
$base = if ($env:DOS_BASE_URL) { $env:DOS_BASE_URL } else { "https://github.com/$repo/releases/download" }
$target = 'x86_64-pc-windows-msvc'

$haveGh = $false
if (Get-Command gh -ErrorAction SilentlyContinue) {
    gh auth status *> $null
    $haveGh = ($LASTEXITCODE -eq 0)
}

if (-not $tag) {
    try { $tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name } catch { }
}
if (-not $tag -and $haveGh) { $tag = (gh release view --repo $repo --json tagName -q .tagName) }
if (-not $tag) { throw "could not find a release of $repo (private repo? run: gh auth login)" }

$asset = "dos-$tag-$target.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dos-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "Downloading $asset ($tag)..."
    try {
        Invoke-WebRequest "$base/$tag/$asset" -OutFile (Join-Path $tmp $asset)
        Invoke-WebRequest "$base/$tag/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")
    } catch {
        if (-not $haveGh) { throw "download failed (private repo? run: gh auth login, then retry)" }
        gh release download $tag --repo $repo --dir $tmp --clobber --pattern $asset --pattern "$asset.sha256"
        if ($LASTEXITCODE -ne 0) { throw "download failed" }
    }

    $want = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $got  = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLower()
    if (-not $want -or $want -ne $got) { throw "checksum mismatch, nothing installed" }
    Write-Host "Checksum OK."

    Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item (Join-Path $tmp 'dos-commander.exe') (Join-Path $dir 'dos-commander.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "Added $dir to your user PATH (open a new terminal)."
}
Write-Host "`nInstalled dos-commander $tag into $dir"
Write-Host "Try it without keys or network:  dos-commander --demo"
