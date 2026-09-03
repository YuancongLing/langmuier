param(
    [string]$ListenAddress = "192.168.1.100",
    [ValidateRange(1, 65535)]
    [int]$ListenPort = 5000,
    [string]$ForwardAddress = "127.0.0.1",
    [ValidateRange(1, 65535)]
    [int]$ForwardPort = 5002,
    [ValidateRange(0, [int]::MaxValue)]
    [int]$Seconds = 0
)

$ErrorActionPreference = "Stop"

$listenIp = [System.Net.IPAddress]::Parse($ListenAddress)
$forwardIp = [System.Net.IPAddress]::Parse($ForwardAddress)
$listenerEndpoint = [System.Net.IPEndPoint]::new($listenIp, $ListenPort)
$forwardEndpoint = [System.Net.IPEndPoint]::new($forwardIp, $ForwardPort)
$listener = [System.Net.Sockets.UdpClient]::new($listenerEndpoint)
$sender = [System.Net.Sockets.UdpClient]::new($forwardIp.AddressFamily)
$sender.Connect($forwardEndpoint)
$listener.Client.ReceiveTimeout = 250
$remoteAddress = if ($listenIp.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetworkV6) {
    [System.Net.IPAddress]::IPv6Any
} else {
    [System.Net.IPAddress]::Any
}

$stopAt = if ($Seconds -gt 0) { (Get-Date).AddSeconds($Seconds) } else { [DateTime]::MaxValue }
$packets = 0L
$bytes = 0L

try {
    Write-Host (
        "Forwarding UDP {0}:{1} to {2}:{3}. Press Ctrl+C to stop." -f `
        $ListenAddress, $ListenPort, $ForwardAddress, $ForwardPort
    ) -ForegroundColor Cyan

    while ((Get-Date) -lt $stopAt) {
        try {
            $remote = [System.Net.IPEndPoint]::new($remoteAddress, 0)
            $data = $listener.Receive([ref]$remote)
        } catch [System.Net.Sockets.SocketException] {
            if ($_.Exception.SocketErrorCode -eq [System.Net.Sockets.SocketError]::TimedOut) {
                continue
            }
            throw
        }

        $sent = $sender.Send($data, $data.Length)
        if ($sent -ne $data.Length) {
            throw "Forwarded only $sent of $($data.Length) bytes"
        }
        $packets++
        $bytes += $data.Length
    }
} finally {
    $sender.Dispose()
    $listener.Dispose()
    Write-Host ("Stopped. Forwarded {0} packets, {1} bytes." -f $packets, $bytes) -ForegroundColor Cyan
}
