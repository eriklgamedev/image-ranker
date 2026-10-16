<#
Attach-only, bounded Windows CPU-memory sampler. No launch, kill, process-name
selection, privilege escalation, deletion, or app configuration changes.
Polling cannot qualify unobserved lifetime peaks; every run reports that limit.
Run only in the execution owner's reserved measurement window.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1,2147483647)][int]$RootProcessId,
    [Parameter(Mandatory)][ValidatePattern('^[0-9]{1,20}$')][string]$RootCreationFileTime,
    [Parameter(Mandatory)][string]$ExpectedExecutable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedSha256,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(1,3600)][int]$DurationSeconds = 120,
    [ValidateRange(25,5000)][int]$IntervalMilliseconds = 100,
    [ValidateRange(1,512)][int]$MaxProcesses = 256,
    [ValidateRange(1,36000)][int]$MaxSamples = 12000,
    [ValidateRange(65536,67108864)][long]$MaxOutputBytes = 33554432,
    [ValidateRange(0,30000)][int]$DrainMilliseconds = 5000
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Windows is required' }

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class QualificationProcess {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Entry {
    public uint Size, Usage, Pid; public IntPtr Heap; public uint Module, Threads, Parent;
    public int Priority; public uint Flags;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst=260)] public string Name;
  }
  [StructLayout(LayoutKind.Sequential)] struct FileTime { public uint Low, High; }
  [StructLayout(LayoutKind.Sequential)] struct Memory {
    public uint Size, Faults; public UIntPtr Peak, Working, PeakPaged, Paged,
      PeakNonPaged, NonPaged, Pagefile, PeakPagefile, Private;
  }
  public sealed class Row { public uint Pid; public uint ParentPid; }
  public sealed class Counters {
    public ulong WorkingSetBytes, PrivateCommitBytes, LifetimePeakWorkingSetBytes;
  }
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateToolhelp32Snapshot(uint flags,uint pid);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool Process32FirstW(IntPtr h,ref Entry e);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool Process32NextW(IntPtr h,ref Entry e);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError=true)] static extern SafeProcessHandle OpenProcess(uint access,bool inherit,uint pid);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(SafeProcessHandle h,out FileTime c,out FileTime x,out FileTime k,out FileTime u);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool QueryFullProcessImageNameW(SafeProcessHandle h,uint flags,StringBuilder text,ref uint length);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(SafeProcessHandle h,uint milliseconds);
  [DllImport("psapi.dll", SetLastError=true)] static extern bool GetProcessMemoryInfo(SafeProcessHandle h,ref Memory info,uint size);
  static Exception Failure(string stage) { return new Win32Exception(Marshal.GetLastWin32Error(), stage); }
  public static SafeProcessHandle Open(uint pid) {
    // Query/read/synchronize only; never PROCESS_TERMINATE or write rights.
    var h=OpenProcess(0x0400|0x0010|0x100000,false,pid);
    if(h.IsInvalid) { var e=Failure("OpenProcess");h.Dispose();throw e; } return h;
  }
  public static string Created(SafeProcessHandle h) {
    FileTime c,x,k,u;if(!GetProcessTimes(h,out c,out x,out k,out u)) throw Failure("GetProcessTimes");
    return (((ulong)c.High<<32)|c.Low).ToString(System.Globalization.CultureInfo.InvariantCulture);
  }
  public static string Image(SafeProcessHandle h) {
    uint n=32768;var s=new StringBuilder((int)n);
    if(!QueryFullProcessImageNameW(h,0,s,ref n)) throw Failure("QueryFullProcessImageName");return s.ToString();
  }
  public static bool Alive(SafeProcessHandle h) {
    var result=WaitForSingleObject(h,0);if(result==258)return true;if(result==0)return false;throw Failure("WaitForSingleObject");
  }
  public static Counters Read(SafeProcessHandle h) {
    Memory m=new Memory();m.Size=(uint)Marshal.SizeOf(typeof(Memory));
    if(!GetProcessMemoryInfo(h,ref m,m.Size))throw Failure("GetProcessMemoryInfo");
    return new Counters {WorkingSetBytes=m.Working.ToUInt64(),PrivateCommitBytes=m.Private.ToUInt64(),LifetimePeakWorkingSetBytes=m.Peak.ToUInt64()};
  }
  public static Row[] Snapshot() {
    var h=CreateToolhelp32Snapshot(2,0);if(h==new IntPtr(-1))throw Failure("CreateToolhelp32Snapshot");
    try {
      var result=new List<Row>();var e=new Entry();e.Size=(uint)Marshal.SizeOf(typeof(Entry));
      if(!Process32FirstW(h,ref e))throw Failure("Process32First");
      do { result.Add(new Row {Pid=e.Pid,ParentPid=e.Parent}); } while(Process32NextW(h,ref e));
      var error=Marshal.GetLastWin32Error();if(error!=18)throw new Win32Exception(error,"Process32Next");
      return result.ToArray();
    } finally { CloseHandle(h); }
  }
}
'@

$qualifiedMembers = [Collections.Generic.Dictionary[int,object]]::new()
$qualificationIssues = [Collections.Generic.HashSet[string]]::new()
$qualificationClock = [Diagnostics.Stopwatch]::StartNew()
$qualificationEncoding = [Text.UTF8Encoding]::new($false)
$qualificationStream = $null
$qualificationBytes = 0L
$qualificationSamples = 0
$qualificationRootEnded = $null
$qualificationStopReason = 'not-started'
$qualificationFatal = $null
$qualificationRootHandle = $null
$qualificationRootPin = $null
$qualificationOutputReady = $false

function Write-QualificationRecord([object]$Record) {
    $qualificationLine = ($Record | ConvertTo-Json -Compress -Depth 8) + "`n"
    $qualificationEncoded = $qualificationEncoding.GetBytes($qualificationLine)
    if ($script:qualificationBytes + $qualificationEncoded.Length -gt $MaxOutputBytes) { return $false }
    $qualificationStream.Write($qualificationEncoded,0,$qualificationEncoded.Length)
    $qualificationStream.Flush($true)
    $script:qualificationBytes += $qualificationEncoded.Length
    return $true
}

try {
    $qualificationExpectedPath = [IO.Path]::GetFullPath($ExpectedExecutable)
    $qualificationRootHandle = [QualificationProcess]::Open([uint32]$RootProcessId)
    if ([QualificationProcess]::Created($qualificationRootHandle) -cne $RootCreationFileTime) { throw 'Root creation time mismatch' }
    $qualificationActualPath = [QualificationProcess]::Image($qualificationRootHandle)
    if (-not [string]::Equals($qualificationActualPath,$qualificationExpectedPath,[StringComparison]::OrdinalIgnoreCase)) { throw 'Root executable path mismatch' }
    # Hold the expected on-disk image against writes/deletion while sampling.
    # This is explicitly on-disk hash evidence; an attach run did not witness launch.
    $qualificationRootPin = [IO.File]::Open($qualificationActualPath,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $qualificationHasher = [Security.Cryptography.SHA256]::Create()
    try { $qualificationHash = [BitConverter]::ToString($qualificationHasher.ComputeHash($qualificationRootPin)).Replace('-','') }
    finally { $qualificationHasher.Dispose() }
    if ($qualificationHash -ine $ExpectedSha256) { throw 'Root executable hash mismatch' }
    if (-not [QualificationProcess]::Alive($qualificationRootHandle)) { throw 'Root already exited' }
    $qualifiedMembers.Add($RootProcessId,[pscustomobject]@{handle=$qualificationRootHandle;created=$RootCreationFileTime;path=$qualificationActualPath;parent=$null;parentCreated=$null})

    $qualificationDirectory = [IO.Path]::GetFullPath($OutputDirectory)
    if (Test-Path -LiteralPath $qualificationDirectory) { throw 'Output directory must be new; evidence is never overwritten' }
    New-Item -ItemType Directory -Path $qualificationDirectory -ErrorAction Stop | Out-Null
    $qualificationStream = [IO.File]::Open((Join-Path $qualificationDirectory 'memory.jsonl'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    $qualificationOutputReady = $true
    [void](Write-QualificationRecord ([ordered]@{type='header';schema=1;startedUtc=[DateTime]::UtcNow.ToString('o');rootPid=$RootProcessId;rootCreated=$RootCreationFileTime;imageSha256OnDisk=$qualificationHash;intervalMs=$IntervalMilliseconds;durationSeconds=$DurationSeconds;maxProcesses=$MaxProcesses;coverage='attach-polling-only';gpuMeasurement='unavailable';samplerPid=$PID;samplerScriptSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash}))

    while ($qualificationClock.Elapsed.TotalSeconds -lt $DurationSeconds -and $qualificationSamples -lt $MaxSamples) {
        $qualificationStartMs = $qualificationClock.Elapsed.TotalMilliseconds
        $qualificationRows = [QualificationProcess]::Snapshot()
        # Resolve parent-first independent of the process snapshot's ordering.
        # An already held child survives parent exit, but a newly seen child of
        # an exited/reused parent cannot be admitted from polling alone.
        for ($qualificationPass=0; $qualificationPass -lt $MaxProcesses; $qualificationPass++) {
            $qualificationAdded = $false
            foreach ($qualificationRow in $qualificationRows) {
                $qualificationPid = [int]$qualificationRow.Pid
                if ($qualifiedMembers.ContainsKey($qualificationPid) -or -not $qualifiedMembers.ContainsKey([int]$qualificationRow.ParentPid)) { continue }
                $qualificationParent = $qualifiedMembers[[int]$qualificationRow.ParentPid]
                if (-not [QualificationProcess]::Alive($qualificationParent.handle)) { [void]$qualificationIssues.Add('unresolved-child-of-exited-parent'); continue }
                if ($qualifiedMembers.Count -ge $MaxProcesses) { [void]$qualificationIssues.Add('process-limit'); break }
                $qualificationChildHandle = $null
                try {
                    $qualificationChildHandle = [QualificationProcess]::Open($qualificationRow.Pid)
                    $qualificationChildCreated = [QualificationProcess]::Created($qualificationChildHandle)
                    if ([UInt64]::Parse($qualificationChildCreated) -lt [UInt64]::Parse($qualificationParent.created) -or -not [QualificationProcess]::Alive($qualificationParent.handle)) {
                        [void]$qualificationIssues.Add('ambiguous-parent-lifetime'); continue
                    }
                    $qualificationChildPath = [QualificationProcess]::Image($qualificationChildHandle)
                    $qualifiedMembers.Add($qualificationPid,[pscustomobject]@{handle=$qualificationChildHandle;created=$qualificationChildCreated;path=$qualificationChildPath;parent=[int]$qualificationRow.ParentPid;parentCreated=$qualificationParent.created})
                    $qualificationChildHandle = $null
                    $qualificationAdded = $true
                } catch { [void]$qualificationIssues.Add('child-query-failed') }
                finally { if ($null -ne $qualificationChildHandle) { $qualificationChildHandle.Dispose() } }
            }
            if (-not $qualificationAdded) { break }
        }

        $qualificationValues = [Collections.Generic.List[object]]::new()
        $qualificationRetired = [Collections.Generic.List[int]]::new()
        $qualificationLive = 0
        foreach ($qualificationMember in $qualifiedMembers.GetEnumerator()) {
            $qualificationOwned = $qualificationMember.Value
            $qualificationValue = [ordered]@{pid=$qualificationMember.Key;created=$qualificationOwned.created;parentPid=$qualificationOwned.parent;parentCreated=$qualificationOwned.parentCreated;imageName=[IO.Path]::GetFileName($qualificationOwned.path);status='unknown'}
            try {
                if (-not [QualificationProcess]::Alive($qualificationOwned.handle)) {
                    $qualificationValue.status='exited'
                    if ($qualificationMember.Key -ne $RootProcessId) { $qualificationRetired.Add($qualificationMember.Key) }
                }
                else {
                    $qualificationLive++
                    $qualificationCounters = [QualificationProcess]::Read($qualificationOwned.handle)
                    $qualificationValue.status='sampled'
                    $qualificationValue.workingSetBytes=$qualificationCounters.WorkingSetBytes
                    $qualificationValue.privateCommitBytes=$qualificationCounters.PrivateCommitBytes
                    $qualificationValue.lifetimePeakWorkingSetBytes=$qualificationCounters.LifetimePeakWorkingSetBytes
                }
            } catch { $qualificationValue.status='query-failed';[void]$qualificationIssues.Add('member-query-failed') }
            $qualificationValues.Add($qualificationValue)
        }
        $qualificationEndMs = $qualificationClock.Elapsed.TotalMilliseconds
        $qualificationRecord = [ordered]@{type='sample';sequence=$qualificationSamples;startMs=$qualificationStartMs;endMs=$qualificationEndMs;processes=@($qualificationValues.ToArray());issues=@($qualificationIssues)}
        if (-not (Write-QualificationRecord $qualificationRecord)) { $qualificationStopReason='output-limit'; break }
        $qualificationSamples++
        # Exit was recorded before dropping its query handle. Keep only live
        # children plus the root; a later reused PID must be admitted afresh.
        foreach ($qualificationRetiredPid in $qualificationRetired) {
            $qualifiedMembers[$qualificationRetiredPid].handle.Dispose()
            [void]$qualifiedMembers.Remove($qualificationRetiredPid)
        }
        if (-not [QualificationProcess]::Alive($qualificationRootHandle) -and $null -eq $qualificationRootEnded) { $qualificationRootEnded=$qualificationEndMs }
        if ($qualificationLive -eq 0) { $qualificationStopReason='observed-members-exited'; break }
        if ($null -ne $qualificationRootEnded -and $qualificationEndMs-$qualificationRootEnded -ge $DrainMilliseconds) { $qualificationStopReason='descendant-drain-deadline'; break }
        $qualificationWait = [Math]::Max(0,$IntervalMilliseconds-($qualificationClock.Elapsed.TotalMilliseconds-$qualificationStartMs))
        if ($qualificationWait -gt 0) { Start-Sleep -Milliseconds ([int]$qualificationWait) }
    }
    if ($qualificationStopReason -eq 'not-started') { $qualificationStopReason=if ($qualificationSamples -ge $MaxSamples) {'sample-limit'} else {'duration-limit'} }
} catch { $qualificationFatal=$_.Exception.Message; $qualificationStopReason='sampler-error' }
finally {
    $qualificationSurvivors = [Collections.Generic.List[object]]::new()
    foreach ($qualificationMember in $qualifiedMembers.GetEnumerator()) {
        try { if ([QualificationProcess]::Alive($qualificationMember.Value.handle)) { $qualificationSurvivors.Add(@{pid=$qualificationMember.Key;created=$qualificationMember.Value.created}) } }
        catch { [void]$qualificationIssues.Add('final-liveness-unknown') }
        finally { $qualificationMember.Value.handle.Dispose() }
    }
    if ($qualifiedMembers.Count -eq 0 -and $null -ne $qualificationRootHandle) { $qualificationRootHandle.Dispose() }
    if ($null -ne $qualificationRootPin) { $qualificationRootPin.Dispose() }
    if ($null -ne $qualificationStream) { $qualificationStream.Dispose() }
    if ($qualificationOutputReady) {
        $qualificationSummary = [ordered]@{schema=1;stopReason=$qualificationStopReason;samples=$qualificationSamples;bytes=$qualificationBytes;coverage='attach-polling-only';peakQualification='not-established';gpuMeasurement='unavailable';fatal=$qualificationFatal;issues=@($qualificationIssues);survivingObservedProcesses=@($qualificationSurvivors.ToArray());processesTerminatedBySampler=0}
        $qualificationSummaryBytes=$qualificationEncoding.GetBytes(($qualificationSummary|ConvertTo-Json -Depth 6))
        $qualificationSummaryFile=[IO.File]::Open((Join-Path $qualificationDirectory 'sampler-summary.json'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
        try {$qualificationSummaryFile.Write($qualificationSummaryBytes,0,$qualificationSummaryBytes.Length);$qualificationSummaryFile.Flush($true)} finally {$qualificationSummaryFile.Dispose()}
    }
}
if ($null -ne $qualificationFatal) { throw $qualificationFatal }
Write-Output ('Captured ' + $qualificationSamples + ' attach-only samples; lifetime peak and GPU qualification remain unestablished. No process was terminated.')
