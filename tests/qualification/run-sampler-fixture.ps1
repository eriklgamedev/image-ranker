<# Executable fixture gate; DO NOT run concurrently with app/native/performance
qualification. It launches only a hidden owned Node tree and the owned sampler.
All evidence is kept. Emergency termination uses held, identity-verified handles.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$NodeExecutable,
  [Parameter(Mandatory)][string]$OutputParent,
  [ValidateRange(20000,90000)][int]$FixtureDeadlineMilliseconds=60000
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if($PSVersionTable.PSEdition -ne 'Core' -or [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Run in Windows PowerShell 7+' }
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public sealed class SamplerFixtureOwner : IDisposable {
  [StructLayout(LayoutKind.Sequential)] struct FT { public uint Low,High; }
  [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] struct Entry {
    public uint Size,Usage,Pid;public IntPtr Heap;public uint Module,Threads,Parent;public int Priority;public uint Flags;
    [MarshalAs(UnmanagedType.ByValTStr,SizeConst=260)]public string Name;
  }
  [DllImport("kernel32.dll",SetLastError=true)] static extern SafeProcessHandle OpenProcess(uint access,bool inherit,uint pid);
  [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetProcessTimes(SafeProcessHandle h,out FT c,out FT x,out FT k,out FT u);
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryFullProcessImageNameW(SafeProcessHandle h,uint flags,StringBuilder b,ref uint size);
  [DllImport("kernel32.dll",SetLastError=true)] static extern uint WaitForSingleObject(SafeProcessHandle h,uint milliseconds);
  [DllImport("kernel32.dll",SetLastError=true)] static extern bool TerminateProcess(SafeProcessHandle h,uint code);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
  [DllImport("kernel32.dll",SetLastError=true)] static extern bool DuplicateHandle(IntPtr sourceProcess,SafeProcessHandle source,IntPtr targetProcess,out SafeProcessHandle target,uint access,bool inherit,uint options);
  [DllImport("kernel32.dll",SetLastError=true)] static extern IntPtr CreateToolhelp32Snapshot(uint flags,uint pid);
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool Process32FirstW(IntPtr h,ref Entry e);
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool Process32NextW(IntPtr h,ref Entry e);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  public sealed class ProcessRow {public uint Pid,ParentPid;}
  SafeProcessHandle handle;public uint Pid{get;private set;}public string Created{get;private set;}public string Image{get;private set;}public uint ParentPid{get;private set;}public string ParentCreated{get;private set;}
  static Exception Error(string stage){return new Win32Exception(Marshal.GetLastWin32Error(),stage);}
  string Creation(){FT c,x,k,u;if(!GetProcessTimes(handle,out c,out x,out k,out u))throw Error("creation");return (((ulong)c.High<<32)|c.Low).ToString();}
  public static ProcessRow[] Snapshot(){var h=CreateToolhelp32Snapshot(2,0);if(h==new IntPtr(-1))throw Error("snapshot");try{
    var rows=new List<ProcessRow>();
    var e=new Entry();e.Size=(uint)Marshal.SizeOf(typeof(Entry));if(!Process32FirstW(h,ref e))throw Error("first");
    do{rows.Add(new ProcessRow{Pid=e.Pid,ParentPid=e.Parent});}while(Process32NextW(h,ref e));
    var error=Marshal.GetLastWin32Error();if(error!=18)throw new Win32Exception(error,"next");return rows.ToArray();
  }finally{CloseHandle(h);}}
  static uint Parent(uint pid){foreach(var row in Snapshot())if(row.Pid==pid)return row.ParentPid;throw new Exception("Fixture process parent not found");}
  public static SamplerFixtureOwner Open(uint pid,string expectedImage,uint expectedParent){
    var owner=new SamplerFixtureOwner();owner.Pid=pid;owner.handle=OpenProcess(0x1000|0x100000|0x0001,false,pid);
    try{
      if(owner.handle.IsInvalid)throw Error("open owned fixture");owner.Created=owner.Creation();
      uint n=32768;var text=new StringBuilder((int)n);if(!QueryFullProcessImageNameW(owner.handle,0,text,ref n))throw Error("image");owner.Image=text.ToString();
      if((expectedImage!=null&&!String.Equals(owner.Image,expectedImage,StringComparison.OrdinalIgnoreCase))||Parent(pid)!=expectedParent)throw new Exception("Fixture native parent/image mismatch");
      owner.ParentPid=expectedParent;
      return owner;
    }catch{owner.Dispose();throw;}
  }
  public static SamplerFixtureOwner Descendant(uint pid,SamplerFixtureOwner parent){
    if(!parent.Alive())throw new Exception("Fixture parent is not live");
    var owner=Open(pid,null,parent.Pid);try{
      if(UInt64.Parse(owner.Created)<UInt64.Parse(parent.Created)||!parent.Alive())throw new Exception("Fixture descendant lifetime mismatch");
      owner.ParentCreated=parent.Created;return owner;
    }catch{owner.Dispose();throw;}
  }
  public static SamplerFixtureOwner Started(uint pid,SafeProcessHandle spawned,string expectedImage){
    // Duplicate the actual Process.Start handle; never reopen a fast-exiting
    // child by its potentially reused numeric PID.
    var owner=new SamplerFixtureOwner();owner.Pid=pid;owner.Image=expectedImage;
    if(!DuplicateHandle(GetCurrentProcess(),spawned,GetCurrentProcess(),out owner.handle,0,false,2))throw Error("duplicate spawned fixture");
    try{owner.Created=owner.Creation();if(owner.Alive()){
      uint n=32768;var image=new StringBuilder((int)n);if(!QueryFullProcessImageNameW(owner.handle,0,image,ref n))throw Error("spawned image");
      if(!String.Equals(image.ToString(),expectedImage,StringComparison.OrdinalIgnoreCase))throw new Exception("Spawned image mismatch");owner.Image=image.ToString();
    }return owner;}catch{owner.Dispose();throw;}
  }
  public bool Alive(){var result=WaitForSingleObject(handle,0);if(result==258)return true;if(result==0)return false;throw Error("wait");}
  public bool Wait(uint ms){var result=WaitForSingleObject(handle,ms);if(result==0)return true;if(result==258)return false;throw Error("wait");}
  public void TerminateExact(){if(Creation()!=Created)throw new Exception("Fixture identity changed");if(Alive()&&!TerminateProcess(handle,117))throw Error("terminate exact fixture handle");}
  public void Dispose(){if(handle!=null)handle.Dispose();}
}
'@

$fixtureClock=[Diagnostics.Stopwatch]::StartNew()
$fixtureNonce=[Guid]::NewGuid().ToString('N')
$fixtureDirectory=Join-Path ([IO.Path]::GetFullPath($OutputParent)) ('sampler-fixture-'+$fixtureNonce)
New-Item -ItemType Directory -Path $fixtureDirectory -ErrorAction Stop | Out-Null
$fixtureNode=[IO.Path]::GetFullPath($NodeExecutable)
$fixturePowerShell=Join-Path $PSHOME 'pwsh.exe'
$fixtureHelper=Join-Path $PSScriptRoot 'tree-helper.mjs'
$fixtureSampler=Join-Path $PSScriptRoot 'windows-process-tree.ps1'
$fixtureCapture=Join-Path $fixtureDirectory 'capture'
$fixtureOwned=[Collections.Generic.List[object]]::new()
$fixtureTreeOwned=[Collections.Generic.List[object]]::new()
$fixtureSpawned=[Collections.Generic.List[object]]::new()
$fixtureAssertions=[Collections.Generic.List[object]]::new()
$fixtureFailure=$null
$fixtureCleanupFailures=[Collections.Generic.List[string]]::new()
$fixtureSamplerOwner=$null
$fixtureRootOwner=$null
$fixtureRootSpawned=$false
$fixtureDeadline=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()+$FixtureDeadlineMilliseconds

function Assert-Fixture([bool]$Condition,[string]$Message) {
  $fixtureAssertions.Add(@{condition=$Message;passed=$Condition})
  if(-not $Condition){throw $Message}
}
function Start-FixtureProcess([string]$Executable,[string[]]$Arguments) {
  $start=[Diagnostics.ProcessStartInfo]::new()
  $start.FileName=$Executable;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
  $start.WindowStyle=[Diagnostics.ProcessWindowStyle]::Hidden
  $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
  foreach($argument in $Arguments){$start.ArgumentList.Add($argument)}
  $process=[Diagnostics.Process]::Start($start)
  $record=[pscustomobject]@{process=$process;stdout=$process.StandardOutput.ReadToEndAsync();stderr=$process.StandardError.ReadToEndAsync()}
  $fixtureSpawned.Add($record)
  return $record
}
function Read-FixtureMessage([string]$Name) {
  $file=Join-Path $fixtureDirectory $Name
  if(-not (Test-Path -LiteralPath $file -PathType Leaf)){return $null}
  if((Get-Item -LiteralPath $file).Length -gt 16384){throw 'Oversize helper handshake'}
  try{$message=Get-Content -LiteralPath $file -Raw|ConvertFrom-Json}catch{return $null}
  if($message.nonce -cne $fixtureNonce){throw 'Helper nonce mismatch'}
  return $message
}
function Wait-Fixture([scriptblock]$Predicate,[string]$Description,[int]$LimitMs=12000) {
  $start=$fixtureClock.ElapsedMilliseconds
  while($fixtureClock.ElapsedMilliseconds-$start -lt $LimitMs -and $fixtureClock.ElapsedMilliseconds -lt $FixtureDeadlineMilliseconds){
    $errorMessage=Read-FixtureMessage 'fixture-error.json'
    if($null -ne $errorMessage){throw ('Helper failed: '+$errorMessage.message)}
    $result=& $Predicate
    if($null -ne $result -and $result -ne $false){return $result}
    Start-Sleep -Milliseconds 50
  }
  throw ('Fixture wait expired: '+$Description)
}
function Write-FixtureCommand([string]$Name){
  $bytes=[Text.Encoding]::UTF8.GetBytes((@{nonce=$fixtureNonce}|ConvertTo-Json -Compress))
  $file=[IO.File]::Open((Join-Path $fixtureDirectory $Name),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
  try{$file.Write($bytes,0,$bytes.Length);$file.Flush($true)}finally{$file.Dispose()}
}
function Read-FixtureSamples {
  $file=Join-Path $fixtureCapture 'memory.jsonl'
  if(-not (Test-Path -LiteralPath $file -PathType Leaf)){return @()}
  $rows=[Collections.Generic.List[object]]::new()
  # The writer shares reads but retains WRITE access. A concurrent read handle
  # must reciprocally share that existing access; ReadAllText's default sharing
  # is incompatible. Do not grant delete sharing or change the writer's gate.
  $reader=[IO.File]::Open($file,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
  try{
    $length=$reader.Length
    if($length -gt 8388608){throw 'Fixture capture exceeded its bound'}
    $bytes=[byte[]]::new([int]$length)
    $offset=0
    while($offset -lt $bytes.Length){
      $count=$reader.Read($bytes,$offset,[Math]::Min(65536,$bytes.Length-$offset))
      if($count -eq 0){throw 'Live fixture evidence shortened during its bounded read'}
      $offset+=$count
    }
  }finally{$reader.Dispose()}
  # Read exactly the observed length, not an unbounded growing stream. A final
  # partial UTF-8/JSON record is left on disk and ignored only by this live poll;
  # the post-exit summarizer still requires an intact final newline.
  $lastNewline=-1
  for($index=$bytes.Length-1;$index -ge 0;$index--){if($bytes[$index] -eq 10){$lastNewline=$index;break}}
  if($lastNewline -lt 0){return @()}
  $text=[Text.UTF8Encoding]::new($false,$true).GetString($bytes,0,$lastNewline+1)
  $lines=$text.Split("`n")
  for($i=0;$i -lt $lines.Length-1;$i++){
    $record=$lines[$i]|ConvertFrom-Json
    if($record.type -eq 'sample'){$rows.Add($record)}
  }
  return $rows.ToArray()
}
function Retain-FixtureNode([object]$Member,[object]$Parent) {
  if($Member.nonce -cne $fixtureNonce -or $Member.parentPid -ne $Parent.Pid){throw 'Reported helper relationship mismatch'}
  if(-not $Parent.Alive()){throw 'Parent exited before child ownership capture'}
  $owner=[SamplerFixtureOwner]::Descendant([uint32]$Member.pid,$Parent)
  $fixtureOwned.Add($owner)
  $fixtureTreeOwned.Add($owner)
  Assert-Fixture ([string]::Equals($owner.Image,$fixtureNode,[StringComparison]::OrdinalIgnoreCase)) 'Reported Node helper has the expected native executable'
  Assert-Fixture ([UInt64]::Parse($owner.Created) -ge [UInt64]::Parse($Parent.Created)) 'Child creation time is within the retained parent lifetime'
  Assert-Fixture ($Member.firstByte -eq 90 -and $Member.lastByte -eq 90 -and $Member.allocationBytes -gt 0) 'Helper reports touched retained allocation'
  return $owner
}
function Capture-FixtureTree {
  # Independent native discovery includes ancillary descendants (for example a
  # console host) by exact live ancestry, never by a name allow/deny list.
  $live=@{}
  foreach($owner in $fixtureTreeOwned){if($owner.Alive()){$live[[uint32]$owner.Pid]=$owner}}
  $rows=[SamplerFixtureOwner]::Snapshot()
  for($pass=0;$pass -lt 12;$pass++){
    $added=$false
    foreach($row in $rows){
      if($live.ContainsKey([uint32]$row.Pid) -or -not $live.ContainsKey([uint32]$row.ParentPid)){continue}
      if($live.Count -ge 12 -or $fixtureTreeOwned.Count -ge 32){throw 'Native fixture tree exceeded its finite live/history ceiling'}
      $owner=[SamplerFixtureOwner]::Descendant($row.Pid,$live[[uint32]$row.ParentPid])
      $fixtureOwned.Add($owner);$fixtureTreeOwned.Add($owner);$live[[uint32]$owner.Pid]=$owner;$added=$true
    }
    if(-not $added){break}
  }
  return @($live.Values)
}
function Capture-StableFixtureTree {
  $last='';$stable=0;$start=$fixtureClock.ElapsedMilliseconds
  while($fixtureClock.ElapsedMilliseconds-$start -lt 4000){
    $tree=@(Capture-FixtureTree)
    $key=(@($tree|ForEach-Object{$_.Pid.ToString()+':'+$_.Created}|Sort-Object)-join ',')
    if($key -ceq $last){$stable++}else{$stable=0;$last=$key}
    if($stable -ge 2){return $tree}
    Start-Sleep -Milliseconds 75
  }
  throw 'Native fixture tree did not stabilize within its finite discovery window'
}
function Fixture-Subtree([object]$Root,[object[]]$Tree){
  $selected=@{($Root.Pid.ToString()+':'+$Root.Created)=$Root}
  for($pass=0;$pass -lt 8;$pass++){
    $added=$false
    foreach($owner in $Tree){
      $key=$owner.Pid.ToString()+':'+$owner.Created
      $parentKey=$owner.ParentPid.ToString()+':'+$owner.ParentCreated
      if(-not $selected.ContainsKey($key) -and $selected.ContainsKey($parentKey)){
        if($selected.Count -ge 8){throw 'Fixture replacement group exceeds finite ownership bound'}
        $selected[$key]=$owner;$added=$true
      }
    }
    if(-not $added){break}
  }
  return @($selected.Values)
}

try {
  $rootProcess=Start-FixtureProcess $fixtureNode @($fixtureHelper,'--role','root','--nonce',$fixtureNonce,'--directory',$fixtureDirectory,'--deadline',[string]$fixtureDeadline)
  $fixtureRootSpawned=$true
  $fixtureRootOwner=[SamplerFixtureOwner]::Started([uint32]$rootProcess.process.Id,$rootProcess.process.SafeHandle,$fixtureNode)
  $fixtureOwned.Add($fixtureRootOwner)
  $fixtureTreeOwned.Add($fixtureRootOwner)
  $ready=Wait-Fixture {Read-FixtureMessage 'tree-ready.json'} 'root/child/grandchild allocation handshake'
  Assert-Fixture ($ready.members.Count -eq 3) 'Initial tree has exactly three members'
  $members=@{}
  foreach($member in $ready.members){if($members.ContainsKey($member.role)){throw 'Duplicate helper role'};$members[$member.role]=$member}
  Assert-Fixture ($members.root.pid -eq $fixtureRootOwner.Pid -and $members.root.parentPid -eq $PID) 'Reported root matches the actually spawned process'
  $childOwner=Retain-FixtureNode $members.child $fixtureRootOwner
  $grandchildOwner=Retain-FixtureNode $members.grandchild $childOwner
  $initialOwners=@(Capture-StableFixtureTree)
  $initialGroup=@(Fixture-Subtree $grandchildOwner $initialOwners)
  $fixtureLiveCap=$initialOwners.Count
  Assert-Fixture ($fixtureLiveCap -ge 3 -and $fixtureLiveCap -le 12) 'Sampler live cap equals the independently qualified initial native tree'
  $identities=@($initialOwners|ForEach-Object{@{pid=$_.Pid;created=$_.Created;parentPid=$_.ParentPid;parentCreated=$_.ParentCreated;image=$_.Image}})
  @{nonce=$fixtureNonce;members=$ready.members;nativeIdentities=$identities;nodeSha256=(Get-FileHash -LiteralPath $fixtureNode -Algorithm SHA256).Hash}|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $fixtureDirectory 'owned-identities.json')

  $samplerArguments=@('-NoProfile','-File',$fixtureSampler,'-RootProcessId',[string]$fixtureRootOwner.Pid,
    '-RootCreationFileTime',$fixtureRootOwner.Created,'-ExpectedExecutable',$fixtureNode,
    '-ExpectedSha256',(Get-FileHash -LiteralPath $fixtureNode -Algorithm SHA256).Hash,
    '-OutputDirectory',$fixtureCapture,'-DurationSeconds','40','-IntervalMilliseconds','100',
    '-MaxProcesses',[string]$fixtureLiveCap,'-MaxOutputBytes','8388608','-DrainMilliseconds','5000')
  $samplerProcess=Start-FixtureProcess $fixturePowerShell $samplerArguments
  $fixtureSamplerOwner=[SamplerFixtureOwner]::Started([uint32]$samplerProcess.process.Id,$samplerProcess.process.SafeHandle,$fixturePowerShell)
  $fixtureOwned.Add($fixtureSamplerOwner)
  $initialSample=Wait-Fixture {
    @(Read-FixtureSamples)|Where-Object { @($_.processes|Where-Object {$_.status -eq 'sampled'}).Count -eq $fixtureLiveCap }|Select-Object -Last 1
  } 'sampler observes the complete qualified native tree and held allocations'
  foreach($owner in $initialOwners){
    $captured=@($initialSample.processes|Where-Object {$_.pid -eq $owner.Pid -and $_.created -ceq $owner.Created})
    Assert-Fixture ($captured.Count -eq 1) 'Each initial process has exactly one matching native creation identity'
    Assert-Fixture ($captured[0].workingSetBytes -gt 0 -and $captured[0].privateCommitBytes -gt 0) 'Working set and private commit are separately positive'
    $declared=@($ready.members|Where-Object {$_.pid -eq $owner.Pid})
    if($declared.Count -eq 1){Assert-Fixture ($captured[0].privateCommitBytes -ge $declared[0].allocationBytes) 'Observed private commit includes the touched held allocation'}
  }
  Write-FixtureCommand 'stop-grandchild.json'
  [void](Wait-Fixture {Read-FixtureMessage 'grandchild-exited.json'} 'owned grandchild cooperative exit')
  foreach($exitedOwner in $initialGroup){
    Assert-Fixture ($exitedOwner.Wait(3000)) 'Exact held original subtree handle confirms OS exit'
    [void](Wait-Fixture {
      @(Read-FixtureSamples)|Where-Object { @($_.processes|Where-Object {$_.pid -eq $exitedOwner.Pid -and $_.created -ceq $exitedOwner.Created -and $_.status -eq 'exited'}).Count -eq 1 }|Select-Object -Last 1
    } 'sampler records each original subtree exit before capacity reuse')
  }
  Write-FixtureCommand 'replace-grandchild.json'
  $replacement=Wait-Fixture {Read-FixtureMessage 'replacement-ready.json'} 'replacement grandchild allocation handshake'
  Assert-Fixture ($replacement.members.Count -eq 1 -and $replacement.members[0].role -eq 'grandchild-2') 'Replacement is the one expected spawned grandchild'
  $replacementOwner=Retain-FixtureNode $replacement.members[0] $childOwner
  Assert-Fixture (($replacementOwner.Pid.ToString()+':'+$replacementOwner.Created) -cne ($grandchildOwner.Pid.ToString()+':'+$grandchildOwner.Created)) 'Replacement never reuses the exited process identity'
  $replacementTree=@(Capture-StableFixtureTree)
  $replacementGroup=@(Fixture-Subtree $replacementOwner $replacementTree)
  Assert-Fixture ($replacementTree.Count -le $fixtureLiveCap) 'Replacement uses the original observed live cap, with no arbitrary increase'
  foreach($newOwner in $replacementGroup){
    [void](Wait-Fixture {
      @(Read-FixtureSamples)|Where-Object { @($_.processes|Where-Object {$_.pid -eq $newOwner.Pid -and $_.created -ceq $newOwner.Created -and $_.status -eq 'sampled' -and $_.workingSetBytes -gt 0 -and $_.privateCommitBytes -gt 0}).Count -eq 1 }|Select-Object -Last 1
    } 'replacement subtree is positively sampled under the original live cap')
  }
  @{initialLiveCap=$fixtureLiveCap;initialSubtree=@($initialGroup|ForEach-Object{@{pid=$_.Pid;created=$_.Created}});replacementSubtree=@($replacementGroup|ForEach-Object{@{pid=$_.Pid;created=$_.Created}});allQualifiedIdentities=@($fixtureTreeOwned|ForEach-Object{@{pid=$_.Pid;created=$_.Created;parentPid=$_.ParentPid;parentCreated=$_.ParentCreated;image=$_.Image}})}|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $fixtureDirectory 'complete-owned-tree.json')
  Write-FixtureCommand 'shutdown.json'
  foreach($owner in $replacementTree){Assert-Fixture ($owner.Wait(6000)) 'Cooperative shutdown reaches actual process exit for every native member'}
  Assert-Fixture ($fixtureSamplerOwner.Wait(8000)) 'Sampler exits after bounded observed descendant drain'
  $samplerProcess.process.WaitForExit()
  Assert-Fixture ($samplerProcess.process.ExitCode -eq 0) 'Sampler interop completes successfully'
  $allSamples=@(Read-FixtureSamples)
  $expectedKeys=@($fixtureTreeOwned|ForEach-Object{$_.Pid.ToString()+':'+$_.Created}|Sort-Object -Unique)
  $actualKeys=@($allSamples|ForEach-Object{$_.processes}|ForEach-Object{[string]$_.pid+':'+$_.created}|Sort-Object -Unique)
  Assert-Fixture (($expectedKeys -join ',') -ceq ($actualKeys -join ',')) 'Captured membership exactly matches all independently qualified fixture descendants'
  Assert-Fixture (@($allSamples|ForEach-Object{$_.issues}|Select-Object -Unique).Count -eq 0) 'Fixture capture has no denied queries, ambiguous membership or capacity loss'

  $summaryProcess=Start-FixtureProcess $fixtureNode @((Join-Path $PSScriptRoot 'summarize-memory.mjs'),$fixtureCapture)
  $summaryOwner=[SamplerFixtureOwner]::Started([uint32]$summaryProcess.process.Id,$summaryProcess.process.SafeHandle,$fixtureNode)
  $fixtureOwned.Add($summaryOwner)
  Assert-Fixture ($summaryOwner.Wait(5000)) 'Bounded memory summarizer exits'
  $summaryProcess.process.WaitForExit()
  Assert-Fixture ($summaryProcess.process.ExitCode -eq 0) 'Captured sampler output passes identity/counter validation'
  $summaryText=$summaryProcess.stdout.GetAwaiter().GetResult()
  $summary=$summaryText|ConvertFrom-Json
  $summaryText|Set-Content -LiteralPath (Join-Path $fixtureDirectory 'interpreted-memory.json')
  Assert-Fixture ($summary.processIdentities -eq $expectedKeys.Count -and $summary.sampledSumWorkingSetPeakBytes -gt 0 -and $summary.sampledSumPrivateCommitPeakBytes -gt 0) 'Interpreted CPU metrics remain separately typed and positive'
  Assert-Fixture ($summary.gpuMeasurement -eq 'unavailable' -and $summary.peakQualification -eq 'not-established' -and $summary.coverage -eq 'attach-polling-only') 'Fixture success does not manufacture GPU or lifetime-peak qualification'
} catch {$fixtureFailure=$_.Exception.Message}
finally {
  # Prefer cooperative own-tree shutdown. Never discover/kill by process name.
  try{if($fixtureRootSpawned -and -not (Test-Path -LiteralPath (Join-Path $fixtureDirectory 'shutdown.json'))){Write-FixtureCommand 'shutdown.json'}}catch{$fixtureCleanupFailures.Add($_.Exception.Message)}
  $fixtureReverseOwners=$fixtureOwned.ToArray()
  [Array]::Reverse($fixtureReverseOwners)
  foreach($owner in $fixtureReverseOwners){
    try{if(-not $owner.Wait(2500)){$owner.TerminateExact();if(-not $owner.Wait(2500)){throw 'Owned fixture failed to exit after exact-handle termination'}}}
    catch{$fixtureCleanupFailures.Add($_.Exception.Message)}
    finally{$owner.Dispose()}
  }
  foreach($record in $fixtureSpawned){
    try{if($record.process.HasExited){$record.stdout.GetAwaiter().GetResult()|Set-Content -LiteralPath (Join-Path $fixtureDirectory ('process-'+$record.process.Id+'.stdout.txt'));$record.stderr.GetAwaiter().GetResult()|Set-Content -LiteralPath (Join-Path $fixtureDirectory ('process-'+$record.process.Id+'.stderr.txt'))}}
    catch{$fixtureCleanupFailures.Add($_.Exception.Message)}
    finally{$record.process.Dispose()}
  }
  @{passed=($null -eq $fixtureFailure -and $fixtureCleanupFailures.Count -eq 0);failure=$fixtureFailure;cleanupFailures=$fixtureCleanupFailures.ToArray();assertions=$fixtureAssertions.ToArray();evidence=$fixtureDirectory}|ConvertTo-Json -Depth 8|Set-Content -LiteralPath (Join-Path $fixtureDirectory 'fixture-result.json')
}
if($null -ne $fixtureFailure -or $fixtureCleanupFailures.Count){throw ('Sampler fixture failed; evidence retained at '+$fixtureDirectory+': '+$fixtureFailure)}
Write-Output ('Sampler helper-tree fixture passed; evidence retained at '+$fixtureDirectory)
