param(
    [Parameter(Mandatory = $true)]
    [string]$Executable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$inputPath = Join-Path $env:TEMP "codextray-hook-$([guid]::NewGuid().ToString('N')).json"
$exitCode = 1
try {
    $utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($inputPath, [Console]::In.ReadToEnd(), $utf8WithoutBom)
    $process = Start-Process `
        -FilePath $Executable `
        -ArgumentList "--hook-event" `
        -RedirectStandardInput $inputPath `
        -PassThru `
        -Wait `
        -WindowStyle Hidden

    $exitCode = $process.ExitCode
} finally {
    Remove-Item -LiteralPath $inputPath -Force -ErrorAction SilentlyContinue
}

exit $exitCode
