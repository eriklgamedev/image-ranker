<#
Prelaunch owned ETW capture. Schedule only in the execution owner's reserved window.
Does not launch the app, elevate, change privileges, stop global sessions or kill.
The retained owner handle is a watchdog: owner exit stops this exact trace.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][ValidateRange(1,2147483647)][int]$OwnerProcessId,
  [Parameter(Mandatory)][ValidatePattern('^[1-9][0-9]{0,19}$')][string]$OwnerCreationFileTime,
  [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{32,64}$')][string]$Nonce,
  [Parameter(Mandatory)][string]$ExpectedExecutable,
  [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExpectedSha256,
  [Parameter(Mandatory)][string]$OutputDirectory,
  [ValidateRange(1,1800)][int]$DurationSeconds=300,
  [ValidateRange(16,1024)][int]$MaximumTraceMiB=256,
  [ValidateRange(65536,67108864)][long]$MaximumDecodedBytes=33554432,
  [ValidateRange(1,250000)][int]$MaximumDecodedEvents=250000
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or [IntPtr]::Size -ne 8) { throw 'Windows x64 required' }
if ($OwnerProcessId -eq $PID) { throw 'The retained execution owner must be separate from the trace controller' }
Add-Type -Path (Join-Path $PSScriptRoot 'WindowsOwnedTrace.cs')

$traceOwner=$null; $traceController=$null; $traceRoot=$null; $traceImagePin=$null; $traceRootImagePin=$null; $traceSession=$null
$traceOutput=[IO.Path]::GetFullPath($OutputDirectory)
$traceEncoding=[Text.UTF8Encoding]::new($false)
$traceIssues=[Collections.Generic.List[string]]::new()
$traceStarted=$false; $traceStopped=$false; $traceDecode=$null; $traceFatal=$null; $traceRootReceipt=$null
$traceReadyTime=$null; $traceControllerCreated=$null; $traceReason='not-started'; $traceLast=$null
$traceStatsFile=$null; $traceOwnerFile=$null; $traceClock=[Diagnostics.Stopwatch]::StartNew()
$traceStatisticsSequence=0; $traceStatisticsBytes=0L

function Write-TraceJson([string]$Name,[object]$Value) {
  $traceBytes=$traceEncoding.GetBytes(($Value | ConvertTo-Json -Depth 10 -Compress)+"`n")
  if ($traceBytes.Length -gt 65536) { throw 'Trace receipt exceeded 64 KiB' }
  $traceTemporary=Join-Path $traceOutput ($Name+'.'+$Nonce+'.pending')
  $traceFile=[IO.File]::Open($traceTemporary,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
  try { $traceFile.Write($traceBytes,0,$traceBytes.Length); $traceFile.Flush($true) } finally { $traceFile.Dispose() }
  [IO.File]::Move($traceTemporary,(Join-Path $traceOutput $Name))
}
function Read-TraceControl([string]$Name,[string[]]$Fields) {
  $traceControlPath=Join-Path $traceOutput $Name
  if (-not [IO.File]::Exists($traceControlPath)) { return $null }
  $traceControlFile=[IO.File]::Open($traceControlPath,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
  try {
    if ($traceControlFile.Length -gt 4096) { throw 'Control receipt exceeds 4 KiB' }
    $traceReader=[IO.StreamReader]::new($traceControlFile,$traceEncoding,$false,4096,$true)
    try { $traceControl=$traceReader.ReadToEnd() | ConvertFrom-Json } finally { $traceReader.Dispose() }
  } finally { $traceControlFile.Dispose() }
  $traceNames=@($traceControl.PSObject.Properties.Name | Sort-Object)
  if (($traceNames -join ',') -cne (($Fields | Sort-Object) -join ',')) { throw "Invalid $Name fields" }
  if ($traceControl.nonce -cne $Nonce -or $traceControl.controllerPid -ne $PID -or $traceControl.controllerCreated -cne $traceControllerCreated) { throw 'Control nonce/controller native identity mismatch' }
  return $traceControl
}
function Write-TraceStatistics([object]$Statistics) {
  $traceRecord=[ordered]@{type='trace-statistics';sequence=$script:traceStatisticsSequence;atFileTime=[DateTime]::UtcNow.ToFileTimeUtc().ToString();elapsedMs=$traceClock.ElapsedMilliseconds;statistics=$Statistics}
  $traceBytes=$traceEncoding.GetBytes(($traceRecord|ConvertTo-Json -Depth 5 -Compress)+"`n")
  if ($script:traceStatisticsBytes+$traceBytes.Length -gt 2097152) { throw 'Trace statistics output bound reached' }
  $traceStatsFile.Write($traceBytes,0,$traceBytes.Length);$traceStatsFile.Flush($true)
  $script:traceStatisticsBytes+=$traceBytes.Length;$script:traceStatisticsSequence++
}

try {
  $traceOwner=[ImageRanker.Qualification.TraceProcess]::Open([uint32]$OwnerProcessId)
  if ([ImageRanker.Qualification.TraceProcess]::Created($traceOwner) -cne $OwnerCreationFileTime -or -not [ImageRanker.Qualification.TraceProcess]::Alive($traceOwner)) { throw 'Execution owner native identity mismatch or exited' }
  $traceController=[ImageRanker.Qualification.TraceProcess]::Open([uint32]$PID)
  $traceControllerCreated=[ImageRanker.Qualification.TraceProcess]::Created($traceController)
  if ([IO.Directory]::Exists($traceOutput) -or [IO.File]::Exists($traceOutput)) { throw 'Trace output directory must be new' }
  [void][IO.Directory]::CreateDirectory($traceOutput)
  $traceOwnerFile=[IO.File]::Open((Join-Path $traceOutput 'controller-owner.lock'),[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::Read)
  Write-TraceJson 'trace-owner.json' ([ordered]@{schema=1;nonce=$Nonce;controllerPid=$PID;controllerCreated=$traceControllerCreated;ownerPid=$OwnerProcessId;ownerCreated=$OwnerCreationFileTime;output=$traceOutput;maximumSeconds=$DurationSeconds;maximumTraceMiB=$MaximumTraceMiB})
  # Pin and hash the executable before readiness, so the qualified path cannot
  # be replaced between app launch and postlaunch process binding.
  $traceImagePin=[ImageRanker.Qualification.TraceProcess]::PinImage($ExpectedExecutable,$ExpectedSha256)
  $traceSession=[ImageRanker.Qualification.OwnedTrace]::new((Join-Path $traceOutput 'process.etl'),[uint32]$MaximumTraceMiB)
  $traceLast=$traceSession.Start();$traceStarted=$true
  $traceStatsFile=[IO.File]::Open((Join-Path $traceOutput 'trace-statistics.jsonl'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
  Write-TraceStatistics $traceLast
  $traceReadyTime=[DateTime]::UtcNow.ToFileTimeUtc().ToString()
  Write-TraceJson 'trace-ready.json' ([ordered]@{schema=1;nonce=$Nonce;controllerPid=$PID;controllerCreated=$traceControllerCreated;readyFileTime=$traceReadyTime;sessionHandle=$traceSession.Handle;sessionGuid=$traceSession.GuidText;sessionName=$traceSession.Name;etl=$traceSession.LogPath;clock='QPC';mode='sequential';maximumTraceMiB=$MaximumTraceMiB;processFlags=9;identityProvider='22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716';identityKeyword='0x10'})
  $traceReason='duration-limit';$traceNextStatistics=1000L
  while ($traceClock.Elapsed.TotalSeconds -lt $DurationSeconds) {
    if (-not [ImageRanker.Qualification.TraceProcess]::Alive($traceOwner)) { $traceReason='owner-exited';break }
    if (-not $traceRootReceipt) {
      $traceRequest=Read-TraceControl 'root-request.json' @('nonce','controllerPid','controllerCreated','pid','created')
      if ($traceRequest) {
        if ($traceRequest.pid -isnot [long] -and $traceRequest.pid -isnot [int]) { throw 'Root PID must be an integer' }
        if ($traceRequest.pid -lt 1 -or $traceRequest.pid -gt 2147483647 -or $traceRequest.created -isnot [string] -or $traceRequest.created -cnotmatch '^[1-9][0-9]{0,19}$') { throw 'Invalid root native identity' }
        if ([uint64]::Parse($traceRequest.created) -lt [uint64]::Parse($traceReadyTime)) { throw 'Root launched before trace readiness' }
        $traceRoot=[ImageRanker.Qualification.TraceProcess]::Open([uint32]$traceRequest.pid)
        if ([ImageRanker.Qualification.TraceProcess]::Created($traceRoot) -cne $traceRequest.created) { throw 'Root creation identity mismatch' }
        $traceRootImagePin=[ImageRanker.Qualification.TraceProcess]::VerifyImage($traceRoot,$ExpectedExecutable,$ExpectedSha256)
        $traceRootReceipt=[ordered]@{nonce=$Nonce;controllerPid=$PID;controllerCreated=$traceControllerCreated;sessionGuid=$traceSession.GuidText;pid=[int]$traceRequest.pid;created=$traceRequest.created;verified=$true;boundFileTime=[DateTime]::UtcNow.ToFileTimeUtc().ToString();executable=[ImageRanker.Qualification.TraceProcess]::Image($traceRoot);sha256=$ExpectedSha256.ToLowerInvariant()}
        Write-TraceJson 'trace-root.json' $traceRootReceipt
      }
    }
    if (Read-TraceControl 'stop-request.json' @('nonce','controllerPid','controllerCreated')) { $traceReason='cooperative-stop';break }
    if ($traceClock.ElapsedMilliseconds -ge $traceNextStatistics) {
      $traceLast=$traceSession.QueryStatistics();Write-TraceStatistics $traceLast
      if ($traceLast.EventsLost -ne 0 -or $traceLast.LogBuffersLost -ne 0 -or $traceLast.RealTimeBuffersLost -ne 0) { $traceReason='event-loss';break }
      if ([IO.File]::Exists($traceSession.LogPath) -and ([IO.FileInfo]$traceSession.LogPath).Length -ge $MaximumTraceMiB*1048576L) { $traceReason='trace-output-limit';break }
      $traceNextStatistics=$traceClock.ElapsedMilliseconds+1000
    }
    Start-Sleep -Milliseconds 100
  }
} catch {
  $traceFatal=$_.Exception.ToString();$traceReason='controller-error'
} finally {
  if ($traceSession -and $traceSession.HasStarted) {
    try {
      $traceLast=$traceSession.StopOwned();$traceStopped=$true
      if ($traceStatsFile) { Write-TraceStatistics $traceLast }
    } catch { $traceIssues.Add('Owned trace stop/query failed: '+$_.Exception.Message) }
  }
  if ($traceStopped) {
    try { $traceDecode=[ImageRanker.Qualification.OwnedTrace]::Decode($traceSession.LogPath,(Join-Path $traceOutput 'trace-events.jsonl'),$MaximumDecodedBytes,$MaximumDecodedEvents) }
    catch { $traceIssues.Add('ETL decode failed: '+$_.Exception.Message) }
  }
  if ($traceOwnerFile) {
    $traceTerminal=[ordered]@{nonce=$Nonce;controllerPid=$PID;controllerCreated=$traceControllerCreated;sessionGuid=$null;stopConfirmed=$traceStopped;decodeComplete=($null -ne $traceDecode -and $traceDecode.Complete);reason=$traceReason;eventsLost=$null;logBuffersLost=$null;realTimeBuffersLost=$null;consumerLost=$null;decodeErrors=$null;issues=@($traceIssues);fatal=$traceFatal}
    if ($traceSession) {$traceTerminal.sessionGuid=$traceSession.GuidText}
    if ($traceLast) {$traceTerminal.eventsLost=$traceLast.EventsLost;$traceTerminal.logBuffersLost=$traceLast.LogBuffersLost;$traceTerminal.realTimeBuffersLost=$traceLast.RealTimeBuffersLost}
    if ($traceDecode) {$traceTerminal.consumerLost=$traceDecode.ConsumerLost;$traceTerminal.decodeErrors=$traceDecode.DecodeErrors}
    try { Write-TraceJson 'trace-terminal.json' $traceTerminal } catch { Write-Error -ErrorAction Continue ('Terminal receipt could not be saved: '+$_.Exception.Message) }
  }
  foreach ($traceResource in @($traceStatsFile,$traceRootImagePin,$traceImagePin,$traceRoot,$traceOwner,$traceController,$traceOwnerFile)) { if ($traceResource) { $traceResource.Dispose() } }
}
if ($traceFatal) { throw $traceFatal }
if (-not $traceStopped -or $traceIssues.Count -ne 0 -or -not $traceRootReceipt) { throw 'Trace capture incomplete; inspect retained receipts. No foreign cleanup attempted.' }
Write-Output ('Owned trace stopped; evidence retained locally at '+$traceOutput)
