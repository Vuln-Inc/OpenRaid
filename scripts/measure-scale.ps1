param(
    [string]$Executable = "$PSScriptRoot\..\target\release\openraid.exe",
    [int[]]$Agents = @(50, 100, 500),
    [string]$DatabaseDirectory = $env:TEMP
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "Build the release executable first: $Executable"
}
if (-not (Test-Path -LiteralPath $DatabaseDirectory -PathType Container)) {
    throw "Database parent directory must already exist: $DatabaseDirectory"
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$results = @()

foreach ($count in $Agents) {
    if ($count -lt 1 -or $count -gt 500) {
        throw 'Agent count must be between 1 and 500'
    }
    $database = Join-Path $DatabaseDirectory "openraid-scale-$count-$([guid]::NewGuid().ToString('N')).sqlite3"
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.Arguments = "demo --agents $count --no-tui --grace-secs 0 --database `"$database`""
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.CreateNoWindow = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) {
        throw 'Unable to start the release executable'
    }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    [long]$peakWorkingSet = 0
    [double]$cpuMs = 0
    [int]$peakThreads = 0
    while (-not $process.HasExited) {
        try {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
            $cpuMs = [Math]::Max($cpuMs, $process.TotalProcessorTime.TotalMilliseconds)
            $peakThreads = [Math]::Max($peakThreads, $process.Threads.Count)
        } catch [System.InvalidOperationException] {
            # The child can finish between an observation and its property query.
        }
        # Sampling cadence, never an operation deadline or cancellation timer.
        [System.Threading.Thread]::Sleep(5)
    }
    $process.WaitForExit()
    $clock.Stop()
    $output = $stdout.GetAwaiter().GetResult()
    $errors = $stderr.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) {
        throw "Scale run failed with exit $($process.ExitCode): $errors"
    }
    $summary = $output | ConvertFrom-Json
    if ($summary.agents -ne $count -or $summary.finished_agents -ne $count) {
        throw 'Scale run did not drain every worker'
    }
    $results += [pscustomobject]@{
        agents = $count
        finished_agents = $summary.finished_agents
        votes = $summary.votes
        board_messages = $summary.board_messages
        harness_elapsed_ms = $summary.elapsed_ms
        process_elapsed_ms = [Math]::Round($clock.Elapsed.TotalMilliseconds, 2)
        sampled_cpu_ms = [Math]::Round($cpuMs, 2)
        peak_working_set_mib = [Math]::Round($peakWorkingSet / 1MB, 2)
        peak_observed_threads = $peakThreads
        database = $database
    }
    $process.Dispose()
}

$results | ConvertTo-Json -Depth 4
