<#
Optional local GPU process-counter observer, at most 1 Hz. It never starts/stops
traces or processes. Counter values are sampled attribution, not lifetime peaks.
Run only after the coordinator has reserved a measurement window.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$TraceDirectory,
  [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{32,64}$')][string]$Nonce,
  [ValidateRange(1,1800)][int]$DurationSeconds=300,
  [ValidateRange(65536,16777216)][long]$MaximumOutputBytes=16777216,
  [ValidateRange(1,512)][int]$MaximumHeldProcesses=256
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
Add-Type -Path (Join-Path $PSScriptRoot 'WindowsOwnedTrace.cs')
$gpuDirectory=[IO.Path]::GetFullPath($TraceDirectory)
$gpuEncoding=[Text.UTF8Encoding]::new($false)
$gpuReadyPath=Join-Path $gpuDirectory 'trace-ready.json'
if (([IO.FileInfo]$gpuReadyPath).Length -gt 65536) { throw 'Oversized trace readiness receipt' }
$gpuReady=[IO.File]::ReadAllText($gpuReadyPath,$gpuEncoding) | ConvertFrom-Json
if ($gpuReady.nonce -cne $Nonce) { throw 'Trace nonce mismatch' }
$gpuController=$null;$gpuOutput=$null;$gpuFatal=$null;$gpuBytes=0L;$gpuSamples=0;$gpuReason='duration-limit'
$gpuHandles=[Collections.Generic.Dictionary[int,object]]::new()
$gpuClock=[Diagnostics.Stopwatch]::StartNew();$gpuNext=0L
try {
  $gpuController=[ImageRanker.Qualification.TraceProcess]::Open([uint32]$gpuReady.controllerPid)
  if ([ImageRanker.Qualification.TraceProcess]::Created($gpuController) -cne $gpuReady.controllerCreated -or -not [ImageRanker.Qualification.TraceProcess]::Alive($gpuController)) { throw 'Trace controller identity mismatch or exited' }
  $gpuOutput=[IO.File]::Open((Join-Path $gpuDirectory 'gpu-samples.jsonl'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
  while ($gpuClock.Elapsed.TotalSeconds -lt $DurationSeconds) {
    if (-not [ImageRanker.Qualification.TraceProcess]::Alive($gpuController) -or [IO.File]::Exists((Join-Path $gpuDirectory 'trace-terminal.json'))) { $gpuReason='controller-finished';break }
    if ($gpuClock.ElapsedMilliseconds -lt $gpuNext) { Start-Sleep -Milliseconds 50;continue }
    $gpuBegin=[DateTime]::UtcNow.ToFileTimeUtc().ToString();$gpuBeginClock=$gpuClock.ElapsedMilliseconds
    $gpuHeldBefore=[Collections.Generic.HashSet[int]]::new()
    foreach ($gpuPair in $gpuHandles.GetEnumerator()) {
      if ([ImageRanker.Qualification.TraceProcess]::Alive($gpuPair.Value.handle)) { [void]$gpuHeldBefore.Add($gpuPair.Key) }
    }
    $gpuCounterErrors=@();$gpuValues=[Collections.Generic.List[object]]::new();$gpuUnresolved=[Collections.Generic.List[object]]::new();$gpuObserved=[Collections.Generic.HashSet[int]]::new();$gpuQueryError=$null
    $gpuCounters=Get-Counter -Counter '\GPU Process Memory(*)\Dedicated Usage','\GPU Process Memory(*)\Shared Usage' -MaxSamples 1 -ErrorAction SilentlyContinue -ErrorVariable gpuCounterErrors
    $gpuEnd=[DateTime]::UtcNow.ToFileTimeUtc().ToString()
    if ($gpuCounterErrors.Count -ne 0) { $gpuQueryError='GPU counter query failed; values are not confirmed' }
    $gpuNativeSamples=@(if ($null -ne $gpuCounters) {$gpuCounters.CounterSamples})
    if ($gpuNativeSamples.Count -eq 0) {$gpuQueryError='GPU counters unavailable; absence is not zero usage'}
    foreach ($gpuSample in $gpuNativeSamples) {
      if ($null -eq $gpuSample) { continue }
      if ($gpuSample.InstanceName -cnotmatch '^pid_([0-9]+)_(luid_0x[0-9a-fA-F]+_0x[0-9a-fA-F]+_phys_[0-9]+)$') { $gpuQueryError='Unsupported GPU instance schema';continue }
      $gpuProcessId=[int]$Matches[1];$gpuAdapter=$Matches[2].ToLowerInvariant()
      if ($gpuProcessId -lt 1) { continue }
      [void]$gpuObserved.Add($gpuProcessId)
      $gpuCounter=if ($gpuSample.Path.EndsWith('\Dedicated Usage',[StringComparison]::OrdinalIgnoreCase)) {'dedicated'} elseif ($gpuSample.Path.EndsWith('\Shared Usage',[StringComparison]::OrdinalIgnoreCase)) {'shared'} else {$null}
      if (-not $gpuCounter) { $gpuQueryError='Unsupported GPU counter path';continue }
      if (-not $gpuHandles.ContainsKey($gpuProcessId) -or -not [ImageRanker.Qualification.TraceProcess]::Alive($gpuHandles[$gpuProcessId].handle)) {
        if ($gpuHandles.ContainsKey($gpuProcessId)) { $gpuHandles[$gpuProcessId].handle.Dispose();[void]$gpuHandles.Remove($gpuProcessId) }
        if ($gpuHandles.Count -ge $MaximumHeldProcesses) { $gpuQueryError='GPU process identity capacity reached';continue }
        try {
          $gpuHandle=[ImageRanker.Qualification.TraceProcess]::Open([uint32]$gpuProcessId)
          try { $gpuCreated=[ImageRanker.Qualification.TraceProcess]::Created($gpuHandle);$gpuHandles.Add($gpuProcessId,[pscustomobject]@{handle=$gpuHandle;created=$gpuCreated}) }
          catch { $gpuHandle.Dispose();throw }
        } catch { $gpuUnresolved.Add([ordered]@{pid=$gpuProcessId;reason='native-process-identity-unavailable'});continue }
        [void]$gpuHeldBefore.Remove($gpuProcessId)
      }
      $gpuOwned=$gpuHandles[$gpuProcessId]
      $gpuValue=[double]$gpuSample.CookedValue
      if ([double]::IsNaN($gpuValue) -or [double]::IsInfinity($gpuValue) -or $gpuValue -lt 0 -or $gpuValue -gt 9007199254740991 -or [Math]::Truncate($gpuValue) -ne $gpuValue) { $gpuQueryError='Invalid GPU byte counter';continue }
      $gpuValues.Add([ordered]@{pid=$gpuProcessId;created=$gpuOwned.created;adapter=$gpuAdapter;counter=$gpuCounter;bytes=$gpuValue.ToString('0',[Globalization.CultureInfo]::InvariantCulture);status=[uint32]$gpuSample.Status;heldBefore=$gpuHeldBefore.Contains($gpuProcessId);aliveAfter=[ImageRanker.Qualification.TraceProcess]::Alive($gpuOwned.handle)})
    }
    foreach ($gpuProcessId in @($gpuHandles.Keys)) {
      if (-not $gpuObserved.Contains($gpuProcessId)) { $gpuHandles[$gpuProcessId].handle.Dispose();[void]$gpuHandles.Remove($gpuProcessId) }
    }
    $gpuRecord=[ordered]@{begin=$gpuBegin;end=$gpuEnd;values=@($gpuValues);unresolved=@($gpuUnresolved);error=$gpuQueryError}
    $gpuEncoded=$gpuEncoding.GetBytes(($gpuRecord|ConvertTo-Json -Depth 6 -Compress)+"`n")
    if ($gpuBytes+$gpuEncoded.Length -gt $MaximumOutputBytes) { $gpuReason='output-limit';break }
    $gpuOutput.Write($gpuEncoded,0,$gpuEncoded.Length);$gpuOutput.Flush($true);$gpuBytes+=$gpuEncoded.Length;$gpuSamples++
    if ($gpuQueryError) { $gpuReason='query-error';break }
    $gpuNext=$gpuBeginClock+1000
  }
} catch { $gpuFatal=$_.Exception.Message;$gpuReason='collector-error' }
finally {
  foreach ($gpuEntry in $gpuHandles.Values) { $gpuEntry.handle.Dispose() }
  if ($gpuController) { $gpuController.Dispose() };if ($gpuOutput) { $gpuOutput.Dispose() }
  $gpuSummary=[ordered]@{schema=1;measurement='unattributed-samples';samples=$gpuSamples;bytes=$gpuBytes;reason=$gpuReason;error=$gpuFatal;lifetimePeakBytes=$null;nativeCaptureQualified=$false;intervalMilliseconds=1000;processesTerminated=0}
  $gpuSummaryBytes=$gpuEncoding.GetBytes(($gpuSummary|ConvertTo-Json -Compress)+"`n")
  $gpuSummaryFile=[IO.File]::Open((Join-Path $gpuDirectory 'gpu-summary.json'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
  try {$gpuSummaryFile.Write($gpuSummaryBytes,0,$gpuSummaryBytes.Length);$gpuSummaryFile.Flush($true)}finally{$gpuSummaryFile.Dispose()}
}
if ($gpuFatal) { throw $gpuFatal }
if ($gpuReason -ne 'controller-finished') { throw ('GPU capture stopped without a completed controller boundary: '+$gpuReason) }
