param(
  [Parameter(Mandatory=$true)][int]$DriverId,
  [Parameter(Mandatory=$true)][string]$ApplicationPath,
  [Parameter(Mandatory=$true)][string]$FixtureRoot,
  [ValidateSet('terminate','inspect','WM_CLOSE')][string]$Action='terminate',
  [int]$ExpectedProcessId=0
)
$ErrorActionPreference='Stop'
$fixture=(Resolve-Path -LiteralPath $FixtureRoot).Path
if((Get-Content -LiteralPath (Join-Path $fixture '.image-ranker-e2e') -Raw).Trim() -ne 'image-ranker-disposable-fixture-v1') { throw 'Not a disposable test fixture' }
$expected=(Resolve-Path -LiteralPath $ApplicationPath).Path
$driver=Get-CimInstance Win32_Process -Filter "ProcessId=$DriverId"
if($driver.Name -ne 'tauri-driver.exe') { throw 'The owned Tauri test driver is not running' }
if($Action -eq 'inspect' -and $ExpectedProcessId -gt 0 -and -not (Get-CimInstance Win32_Process -Filter "ProcessId=$ExpectedProcessId")) {
  # No window/ancestry remains to inspect after exit. This is a read-only report
  # for the previously verified exact PID, including if its Edge driver exited.
  @{action=$Action;running=$false;processId=$ExpectedProcessId;application=$expected;fixture=$fixture;driverPid=$DriverId;edgeDriverPid=$null}|ConvertTo-Json -Compress
  exit 0
}
$edges=@(Get-CimInstance Win32_Process -Filter "ParentProcessId=$DriverId" | Where-Object {$_.Name -eq 'msedgedriver.exe'})
if($edges.Count -ne 1) { throw 'Cannot identify exactly one owned Edge test driver' }
$edgeId=$edges[0].ProcessId
$apps=@(Get-CimInstance Win32_Process -Filter "ParentProcessId=$edgeId" | Where-Object {$_.Name -eq 'image-ranker.exe' -and $_.ExecutablePath -eq $expected})
if($apps.Count -eq 0 -and $Action -eq 'inspect' -and $ExpectedProcessId -gt 0) {
  # A surviving/reused PID outside this ancestry is never accepted as an exit.
  $remaining=Get-CimInstance Win32_Process -Filter "ProcessId=$ExpectedProcessId"
  if($remaining) { throw 'The expected application PID still exists outside the verified ancestry' }
  @{action=$Action;running=$false;processId=$ExpectedProcessId;application=$expected;fixture=$fixture;driverPid=$DriverId;edgeDriverPid=$edgeId}|ConvertTo-Json -Compress
  exit 0
}
if($apps.Count -ne 1) { throw 'Cannot identify exactly one owned instrumented application' }
$application=$apps[0]
$applicationId=[int]$application.ProcessId
if($ExpectedProcessId -gt 0 -and $ExpectedProcessId -ne $applicationId) { throw 'The owned application changed after the prior verification' }
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ImageRankerE2E {
  public static class NativeWindow {
    [DllImport("user32.dll", SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll", SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
  }
}
'@
$nativeProcess=Get-Process -Id $applicationId -ErrorAction SilentlyContinue
if(-not $nativeProcess) {
  if($Action -eq 'inspect' -and $ExpectedProcessId -eq $applicationId) {
    @{action=$Action;running=$false;processId=$applicationId;application=$expected;fixture=$fixture;driverPid=$DriverId;edgeDriverPid=$edgeId}|ConvertTo-Json -Compress
    exit 0
  }
  throw 'The verified application exited before the action'
}
$window=$nativeProcess.MainWindowHandle
$windowAvailable=$window -ne [IntPtr]::Zero -and [ImageRankerE2E.NativeWindow]::IsWindow($window)
if(-not $windowAvailable -and $Action -ne 'inspect') { throw 'The owned application has no verified native main window' }
[uint32]$windowProcessId=0
if($windowAvailable) {
  $threadId=[ImageRankerE2E.NativeWindow]::GetWindowThreadProcessId($window,[ref]$windowProcessId)
  if($threadId -eq 0 -or $windowProcessId -ne $applicationId) {
    if($Action -eq 'inspect' -and -not [ImageRankerE2E.NativeWindow]::IsWindow($window)) { $windowAvailable=$false }
    else { throw 'The native window does not belong to the verified application PID' }
  }
}
# Recheck process creation times, executable and ancestry immediately before
# acting, so PID reuse cannot widen the action to an unrelated process/window.
$current=Get-CimInstance Win32_Process -Filter "ProcessId=$applicationId"
$currentEdge=Get-CimInstance Win32_Process -Filter "ProcessId=$edgeId"
$currentDriver=Get-CimInstance Win32_Process -Filter "ProcessId=$DriverId"
if(-not $current -and $Action -eq 'inspect' -and $ExpectedProcessId -eq $applicationId) {
  @{action=$Action;running=$false;processId=$applicationId;application=$expected;fixture=$fixture;driverPid=$DriverId;edgeDriverPid=$edgeId}|ConvertTo-Json -Compress
  exit 0
}
if(-not $current -or $current.CreationDate -ne $application.CreationDate -or $current.ParentProcessId -ne $edgeId -or $current.ExecutablePath -ne $expected -or $current.Name -ne 'image-ranker.exe') { throw 'Application identity changed before the action' }
if(-not $currentEdge -or $currentEdge.CreationDate -ne $edges[0].CreationDate -or $currentEdge.ParentProcessId -ne $DriverId -or $currentEdge.Name -ne 'msedgedriver.exe') { throw 'Edge driver identity changed before the action' }
if(-not $currentDriver -or $currentDriver.CreationDate -ne $driver.CreationDate -or $currentDriver.Name -ne 'tauri-driver.exe') { throw 'Tauri driver identity changed before the action' }
[uint32]$finalWindowProcessId=0
if($windowAvailable) {
  $null=[ImageRankerE2E.NativeWindow]::GetWindowThreadProcessId($window,[ref]$finalWindowProcessId)
  if($finalWindowProcessId -ne $applicationId -or -not [ImageRankerE2E.NativeWindow]::IsWindow($window)) {
    if($Action -eq 'inspect' -and -not [ImageRankerE2E.NativeWindow]::IsWindow($window)) { $windowAvailable=$false }
    else { throw 'Window identity changed before the action' }
  }
}
# During normal shutdown the window can disappear before the process exits.
# Read-only inspection reports that state; close/terminate always require HWND verification.
$result=@{action=$Action;running=$true;processId=$applicationId;windowAvailable=$windowAvailable;windowPid=$finalWindowProcessId;windowHandle=$window.ToInt64().ToString();application=$expected;fixture=$fixture;driverPid=$DriverId;edgeDriverPid=$edgeId}
if($Action -eq 'terminate') {
  Stop-Process -Id $applicationId -Force
  Wait-Process -Id $applicationId -Timeout 10 -ErrorAction SilentlyContinue
  if(Get-Process -Id $applicationId -ErrorAction SilentlyContinue) { throw 'The verified application did not exit after termination' }
  $result.running=$false
  $result.terminatedPid=$applicationId
} elseif($Action -eq 'WM_CLOSE') {
  if(-not [ImageRankerE2E.NativeWindow]::PostMessage($window,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)) { throw "WM_CLOSE could not be posted: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
  $result.closeRequestedPid=$applicationId
}
$result|ConvertTo-Json -Compress
