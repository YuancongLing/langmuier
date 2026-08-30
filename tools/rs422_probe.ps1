param(
    [Parameter(Mandatory = $false)]
    [string]$Port = "COM3",
    [int]$BaudRate = 115200,
    [int]$Seconds = 15,
    [int]$RequestIntervalMs = 1000,
    [string]$CommandHex = "",
    [switch]$SendOnce
)

$ErrorActionPreference = "Stop"

function Convert-ToHex([byte[]]$Bytes) {
    return (($Bytes | ForEach-Object { $_.ToString("X2") }) -join " ")
}

function Convert-FromHex([string]$Text) {
    $compact = $Text -replace "[\s_]", ""
    if ($compact.StartsWith("0x", [System.StringComparison]::OrdinalIgnoreCase)) {
        $compact = $compact.Substring(2)
    }
    if ($compact.Length -eq 0 -or $compact.Length % 2 -ne 0) {
        throw "CommandHex must contain complete hexadecimal bytes"
    }
    $bytes = New-Object byte[] ($compact.Length / 2)
    for ($index = 0; $index -lt $bytes.Length; $index++) {
        $bytes[$index] = [Convert]::ToByte($compact.Substring($index * 2, 2), 16)
    }
    return $bytes
}

function Remove-Noise([System.Collections.Generic.List[byte]]$Buffer) {
    while ($Buffer.Count -ge 2) {
        $headerIndex = -1
        for ($index = 0; $index -lt ($Buffer.Count - 1); $index++) {
            if ($Buffer[$index] -eq 0xEB -and $Buffer[$index + 1] -eq 0x90) {
                $headerIndex = $index
                break
            }
        }
        if ($headerIndex -lt 0) {
            $retain = if ($Buffer[$Buffer.Count - 1] -eq 0xEB) { 1 } else { 0 }
            if ($Buffer.Count -gt $retain) {
                $Buffer.RemoveRange(0, $Buffer.Count - $retain)
            }
            return $false
        }
        if ($headerIndex -gt 0) {
            $Buffer.RemoveRange(0, $headerIndex)
        }
        return $true
    }
    return $false
}

function Parse-Frames([System.Collections.Generic.List[byte]]$Buffer) {
    while (Remove-Noise $Buffer) {
        if ($Buffer.Count -lt 64) {
            return
        }
        $frame = [byte[]]$Buffer.GetRange(0, 64).ToArray()
        if ($frame[2] -ne 0x3A -or $frame[3] -ne 0x04 -or $frame[4] -ne 0x12 -or
            $frame[62] -ne 0x09 -or $frame[63] -ne 0xD7) {
            Write-Host ("RX candidate rejected: " + (Convert-ToHex $frame)) -ForegroundColor Yellow
            $Buffer.RemoveAt(0)
            continue
        }

        $sum = 0
        for ($index = 3; $index -lt 61; $index++) {
            $sum = ($sum + [int]$frame[$index]) -band 0xFF
        }
        if ($frame[61] -ne $sum) {
            Write-Host ("RX checksum mismatch: expected {0:X2}, actual {1:X2}" -f $sum, $frame[61]) -ForegroundColor Red
            $Buffer.RemoveAt(0)
            continue
        }

        $lastCommand = Convert-ToHex ([byte[]]$frame[34..42])
        $absoluteTime = [uint32]::Parse((Convert-ToHex ([byte[]]$frame[7..10])).Replace(" ", ""), [Globalization.NumberStyles]::HexNumber)
        $localTime = [uint32]::Parse((Convert-ToHex ([byte[]]$frame[11..14])).Replace(" ", ""), [Globalization.NumberStyles]::HexNumber)
        Write-Host ("RX telemetry OK: accepted={0}, rejected={1}, absolute_time={2}, local_time={3}, last_command={4}" -f $frame[18], $frame[19], $absoluteTime, $localTime, $lastCommand) -ForegroundColor Green
        Write-Host ("RX raw: " + (Convert-ToHex $frame))
        $Buffer.RemoveRange(0, 64)
    }
}

$defaultRequest = [byte[]](
    0xEB, 0x90, 0x0A, 0x04, 0x11,
    0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B,
    0xED, 0x09, 0xD7
)
$request = if ([string]::IsNullOrWhiteSpace($CommandHex)) {
    $defaultRequest
} else {
    Convert-FromHex $CommandHex
}
if ($request.Length -ne 16) {
    throw "RS422 command must contain exactly 16 bytes, got $($request.Length)"
}
$buffer = [System.Collections.Generic.List[byte]]::new()
$serial = [System.IO.Ports.SerialPort]::new(
    $Port,
    $BaudRate,
    [System.IO.Ports.Parity]::None,
    8,
    [System.IO.Ports.StopBits]::One
)
$serial.ReadTimeout = 100
$serial.WriteTimeout = 1000

try {
    Write-Host ("Opening {0} at {1} baud for {2} seconds" -f $Port, $BaudRate, $Seconds)
    $serial.Open()
    $stopAt = (Get-Date).AddSeconds($Seconds)
    $nextRequest = Get-Date
    while ((Get-Date) -lt $stopAt) {
        if ((Get-Date) -ge $nextRequest) {
            $serial.Write($request, 0, $request.Length)
            Write-Host ("TX command: " + (Convert-ToHex $request)) -ForegroundColor Cyan
            $nextRequest = if ($SendOnce) {
                $stopAt
            } else {
                (Get-Date).AddMilliseconds($RequestIntervalMs)
            }
        }
        while ($serial.BytesToRead -gt 0) {
            try {
                [void]$buffer.Add([byte]$serial.ReadByte())
            } catch [TimeoutException] {
                break
            }
        }
        Parse-Frames $buffer
        Start-Sleep -Milliseconds 20
    }
    if ($buffer.Count -gt 0) {
        Write-Host ("Unparsed trailing bytes ({0}): {1}" -f $buffer.Count, (Convert-ToHex $buffer.ToArray())) -ForegroundColor Yellow
    }
} catch {
    Write-Error $_
} finally {
    if ($serial.IsOpen) {
        $serial.Close()
    }
    $serial.Dispose()
}
