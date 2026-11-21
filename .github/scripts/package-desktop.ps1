param(
    [Parameter(Mandatory)] [string] $BinaryDirectory,
    [Parameter(Mandatory)] [string] $Archive
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$archivePath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Archive)
$destination = Split-Path -Parent $archivePath
$stage = Join-Path $destination ('stage-' + (Split-Path -Leaf $archivePath))
$bin = New-Item -ItemType Directory -Force -Path (Join-Path $stage 'bin')
$suffix = if ($IsWindows) { '.exe' } else { '' }

$executables = foreach ($name in @('reel', 'reel-lsp', 'reel-game')) {
    $filename = "$name$suffix"
    Copy-Item -LiteralPath (Join-Path $BinaryDirectory $filename) -Destination $bin.FullName
    Join-Path $bin.FullName $filename
}

if ($IsWindows) {
    Compress-Archive -LiteralPath $bin.FullName -DestinationPath $archivePath -Force
} else {
    chmod 755 $executables
    tar -czf $archivePath -C $stage bin
}
