param(
    [string]$LocalAddress = "0.0.0.0",
    [int]$Port = 5000,
    [int]$Seconds = 0,
    [string]$OutputFile = "",
    [switch]$Ascii
)

$ErrorActionPreference = "Stop"

function Convert-ToHex([byte[]]$Bytes) {
    return (($Bytes | ForEach-Object { $_.ToString("X2") }) -join " ")
}

function Convert-ToPrintableAscii([byte[]]$Bytes) {
    return -join ($Bytes | ForEach-Object {
        if ($_ -ge 0x20 -and $_ -le 0x7E) {
            [char]$_
        } else {
            "."
        }
    })
}

if ($Port -lt 1 -or $Port -gt 65535) {
    throw "Port must be between 1 and 65535"
}
if ($Seconds -lt 0) {
    throw "Seconds must be zero (run until Ctrl+C) or a positive duration"
}
$address = [System.Net.IPAddress]::Parse($LocalAddress)
$endpoint = [System.Net.IPEndPoint]::new($address, $Port)
$udp = [System.Net.Sockets.UdpClient]::new($endpoint)
$udp.Client.ReceiveTimeout = 250
$stopAt = if ($Seconds -gt 0) { (Get-Date).AddSeconds($Seconds) } else { [DateTime]::MaxValue }
$packetCount = 0L
$byteCount = 0L
$writer = $null

try {
    if (-not [string]::IsNullOrWhiteSpace($OutputFile)) {
        $outputPath = [System.IO.Path]::GetFullPath($OutputFile)
        $writer = [System.IO.StreamWriter]::new($outputPath, $true, [System.Text.UTF8Encoding]::new($false))
        $writer.AutoFlush = $true
        Write-Host "Decoded packet log: $outputPath"
    }

    Write-Host ("Listening for UDP data on {0}:{1}. Press Ctrl+C to stop." -f $LocalAddress, $Port) -ForegroundColor Cyan
    while ((Get-Date) -lt $stopAt) {
        try {
            $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
            $data = $udp.Receive([ref]$remote)
        } catch [System.Net.Sockets.SocketException] {
            if ($_.Exception.SocketErrorCode -eq [System.Net.Sockets.SocketError]::TimedOut) {
                continue
            }
            throw
        }

        $packetCount++
        $byteCount += $data.Length
        $timestamp = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss.fff")
        $record = "[{0}] #{1} {2}:{3}  {4} bytes  {5}" -f `
            $timestamp, $packetCount, $remote.Address, $remote.Port, $data.Length, (Convert-ToHex $data)
        Write-Host $record
        if ($Ascii) {
            Write-Host ("  ASCII: " + (Convert-ToPrintableAscii $data)) -ForegroundColor DarkGray
        }
        if ($null -ne $writer) {
            $writer.WriteLine($record)
        }
    }
} finally {
    if ($null -ne $writer) {
        $writer.Dispose()
    }
    $udp.Dispose()
    Write-Host ("Stopped. Received {0} packets, {1} bytes." -f $packetCount, $byteCount) -ForegroundColor Cyan
}
