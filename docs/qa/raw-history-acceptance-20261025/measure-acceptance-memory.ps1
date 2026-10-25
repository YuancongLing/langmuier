param([int]$TargetProcessId, [string]$OutputPath)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $OutputPath) { throw 'Refusing existing memory evidence path' }
$samples = @()
$measureStarted = Get-Date
foreach ($sampleIndex in 0..11) {
    $measuredProcess = Get-Process -Id $TargetProcessId
    $measuredProcess.Refresh()
    $samples += [pscustomobject]@{
        elapsed_seconds = ((Get-Date) - $measureStarted).TotalSeconds
        pid = $measuredProcess.Id
        responding = $measuredProcess.Responding
        working_set_bytes = $measuredProcess.WorkingSet64
        private_memory_bytes = $measuredProcess.PrivateMemorySize64
    }
    if ($sampleIndex -lt 11) { Start-Sleep -Seconds 5 }
}
[pscustomobject]@{
    label = 'SYNTHETIC'
    scope = 'Short localhost raw recording observation; no hardware or long soak claim'
    elapsed_seconds = ((Get-Date) - $measureStarted).TotalSeconds
    samples = $samples
} | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 -LiteralPath $OutputPath
$samples | Measure-Object -Property working_set_bytes -Minimum -Maximum
$samples | Measure-Object -Property private_memory_bytes -Minimum -Maximum
$samples | Select-Object elapsed_seconds, responding
