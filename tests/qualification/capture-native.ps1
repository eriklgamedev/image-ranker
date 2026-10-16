param(
  [Parameter(Mandatory=$true)][ValidatePattern('^[a-z0-9-]+$')][string]$Label,
  [string]$Baseline='tests-artifacts/restore-worker-qualified-inputs.json',
  [switch]$E2e
)
$ErrorActionPreference='Stop'
$captureWorkspace=(Get-Location).Path
$captureBase=Get-Content -LiteralPath $Baseline -Raw | ConvertFrom-Json
$capturePaths=@($captureBase.inputs.path)+@(Get-ChildItem -LiteralPath 'src-tauri/src' -Recurse -File | ForEach-Object {
  [IO.Path]::GetRelativePath($captureWorkspace,$_.FullName).Replace('\','/')
})
$capturePaths+=@(Get-ChildItem -LiteralPath 'src-tauri/icons' -Recurse -File | ForEach-Object {
  [IO.Path]::GetRelativePath($captureWorkspace,$_.FullName).Replace('\','/')
})
$capturePaths+=@(Get-ChildItem -LiteralPath 'src-tauri/install-lifetime-core/src' -Recurse -File | ForEach-Object {
  [IO.Path]::GetRelativePath($captureWorkspace,$_.FullName).Replace('\','/')
})
$capturePaths+=@('src-tauri/install-lifetime-core/Cargo.toml')
foreach($captureOptional in @('src-tauri/install-lifetime-core/Cargo.lock','src-tauri/install-lifetime-core/build.rs','src-tauri/install-lifetime-core/.cargo/config.toml')) {
  if(Test-Path -LiteralPath $captureOptional) {$capturePaths+=@($captureOptional)}
}
$captureInputs=@($capturePaths | Sort-Object -Unique | ForEach-Object {
  [ordered]@{path=$_;sha256=(Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()}
})
$captureFeatures=@(if($E2e){'e2e'})
$captureRecord=[ordered]@{buildStartedAtUtc=[DateTime]::UtcNow.ToString('o');status='building';features=$captureFeatures;inputs=$captureInputs}
$capturePath=Join-Path 'tests-artifacts' ($Label+'-inputs.json')
$captureRecord | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $capturePath
$captureStarted=[DateTime]::Parse($captureRecord.buildStartedAtUtc).ToUniversalTime()
$env:CARGO_HOME=Join-Path $captureWorkspace '.cargo-home'
$env:RUSTUP_TOOLCHAIN='stable'
if ((rustc --version) -notmatch '^rustc 1\.94\.1 ') {throw 'Toolchain differs from project pin'}
$captureArguments=@('test','--manifest-path','src-tauri/Cargo.toml','--lib','--offline','--locked','--no-run')
if($E2e) {$captureArguments+=@('--features','e2e')}
cargo @captureArguments 2>&1 |
  Tee-Object -FilePath (Join-Path 'tests-artifacts' ($Label+'-compile.log'))
$captureExit=$LASTEXITCODE
foreach($captureInput in $captureInputs) {
  if((Get-FileHash -LiteralPath $captureInput.path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $captureInput.sha256) {
    throw ('Captured input changed: '+$captureInput.path)
  }
}
$captureRecord.compileExitCode=$captureExit
if($captureExit -ne 0) {
  $captureRecord.status='compile-failed'
  $captureRecord | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $capturePath
  exit $captureExit
}
$captureBinaries=@(Get-ChildItem -LiteralPath 'src-tauri/target/debug/deps' -Filter 'image_ranker_lib-*.exe' |
  Where-Object {$_.LastWriteTimeUtc -ge $captureStarted})
if($captureBinaries.Count -eq 0 -and (($captureBase.features -join ',') -eq ($captureFeatures -join ',')) -and $captureBase.binaryPath -and
   (Test-Path -LiteralPath $captureBase.binaryPath) -and
   (Get-FileHash -LiteralPath $captureBase.binaryPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq $captureBase.binarySha256) {
  # Cargo may reuse a binary when only a feature-excluded module changed. Reuse
  # only the exact previously captured object, after successful locked no-run.
  $captureBinaries=@(Get-Item -LiteralPath $captureBase.binaryPath)
  $captureRecord.reusedVerifiedBinary=$true
} elseif($captureBinaries.Count -ne 1) {throw 'Expected one rebuilt or verified cached native binary'}
$captureRecord.status='compiled-inputs-unchanged'
$captureRecord.binaryPath=$captureBinaries[0].FullName
$captureRecord.binarySha256=(Get-FileHash -LiteralPath $captureBinaries[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$captureRecord | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $capturePath
Write-Output ('Captured '+$captureRecord.binarySha256+' with '+$captureInputs.Count+' inputs')
