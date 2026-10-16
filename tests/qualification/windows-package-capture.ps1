# Reusable internal package capture. Running this script BUILDS but never launches or installs the application.
# Requires PowerShell 7, a separately granted exclusive build window, and already installed dependencies/tools.
[CmdletBinding()]
param(
  [ValidatePattern('^[a-z0-9][a-z0-9-]{0,40}$')][string]$Label='production',
  [Parameter(Mandatory=$true)][switch]$ExclusiveBuildWindow
)
$ErrorActionPreference='Stop'
if (-not $IsWindows -or $PSVersionTable.PSVersion.Major -lt 7) { throw 'Windows PowerShell 7 is required' }
if (-not $ExclusiveBuildWindow) { throw 'An exclusive build window is required: beforeBuild rewrites the shared dist directory' }
$packageWorkspace=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$packageId='windows-package-'+$Label+'-'+[DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffZ')+'-'+[Guid]::NewGuid().ToString('N')
$packageEvidence=Join-Path $packageWorkspace ('tests-artifacts/'+$packageId)
$packageTarget=Join-Path $packageWorkspace ('.qualification-targets/p-'+[Guid]::NewGuid().ToString('N'))
$packageCargoHome=Join-Path $packageWorkspace '.cargo-home'
$packageHelper=Join-Path $PSScriptRoot 'package-capture.mjs'
$packageOverlay=Join-Path $packageWorkspace 'src-tauri/tauri.windows-qualification.conf.json'
$packageManifest=Join-Path $packageWorkspace 'src-tauri/Cargo.toml'
$packageCommands=[Collections.Generic.List[object]]::new()
$packageTools=[Collections.Generic.List[object]]::new()
$packageFailures=[Collections.Generic.List[string]]::new()
$packageStatus='preflight-failed'
$packageBuildExit=$null
$packageInputCaptured=$false
$packageIdentityVerified=$false
$packageVerification=$null
$packageLock=$null
$packageStarted=[DateTime]::UtcNow.ToString('o')
foreach($directory in @($packageEvidence,$packageTarget)) {
  if (-not $directory.StartsWith($packageWorkspace+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) {throw 'Output escaped the workspace'}
  if (Test-Path -LiteralPath $directory) {throw 'Refusing to reuse an existing output directory'}
  $null=New-Item -ItemType Directory -Path $directory
}
function Write-PackageJson([string]$Path,[object]$Value) {
  $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 50)+[Environment]::NewLine)
  $stream=[IO.File]::Open($Path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
  try {$stream.Write($bytes,0,$bytes.Length);$stream.Flush($true)} finally {$stream.Dispose()}
}
function Resolve-PackageTool([string]$Name) {
  $command=Get-Command $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1
  $file=$command.Source
  $packageTools.Add([ordered]@{name=$Name;path=$file;sha256=(Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()})
  return $file
}
# FIX 1: Fixed structured process arguments and a per-child environment. Nothing is interpolated into shell code.
function Invoke-PackageCommand([string]$Name,[string]$Executable,[string[]]$Arguments,[switch]$AllowFailure) {
  $ordinal=$packageCommands.Count.ToString('00')
  $stdout=Join-Path $packageEvidence ($ordinal+'-'+$Name+'.stdout.log')
  $stderr=Join-Path $packageEvidence ($ordinal+'-'+$Name+'.stderr.log')
  $record=[ordered]@{name=$Name;executable=$Executable;arguments=@($Arguments);startedAt=[DateTime]::UtcNow.ToString('o');stdout=$stdout;stderr=$stderr;pid=$null;processStartedAt=$null;exitCode=$null;launchError=$null}
  $packageCommands.Add($record)
  $info=[Diagnostics.ProcessStartInfo]::new()
  $info.FileName=$Executable;$info.WorkingDirectory=$packageWorkspace
  $info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.WindowStyle=[Diagnostics.ProcessWindowStyle]::Hidden
  $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
  foreach($argument in $Arguments){$info.ArgumentList.Add($argument)}
  $info.Environment['RUSTUP_TOOLCHAIN']='stable'
  $info.Environment['CARGO_HOME']=$packageCargoHome
  $info.Environment['CARGO_TARGET_DIR']=$packageTarget
  $info.Environment['CARGO_NET_OFFLINE']='true'
  $info.Environment['npm_config_offline']='true'
  $info.Environment['npm_config_audit']='false'
  $info.Environment['npm_config_fund']='false'
  $info.Environment['CI']='true'
  $info.Environment['NO_COLOR']='1'
  $process=[Diagnostics.Process]::new();$process.StartInfo=$info
  $outFile=$null;$errFile=$null
  try {
    $outFile=[IO.File]::Open($stdout,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    $errFile=[IO.File]::Open($stderr,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    if(-not $process.Start()){throw 'Process start returned false'}
    $record.pid=$process.Id;$record.processStartedAt=$process.StartTime.ToUniversalTime().ToString('o')
    $copyOut=$process.StandardOutput.BaseStream.CopyToAsync($outFile)
    $copyErr=$process.StandardError.BaseStream.CopyToAsync($errFile)
    $process.WaitForExit()
    $copyOut.GetAwaiter().GetResult();$copyErr.GetAwaiter().GetResult()
    $outFile.Flush($true);$errFile.Flush($true)
    $record.exitCode=$process.ExitCode
  } catch {
    $record.launchError=$_.Exception.Message
    throw
  } finally {
    $record.finishedAt=[DateTime]::UtcNow.ToString('o')
    if($outFile){$outFile.Dispose()};if($errFile){$errFile.Dispose()};$process.Dispose()
  }
  if($record.exitCode -ne 0 -and -not $AllowFailure){throw ($Name+' failed with exit '+$record.exitCode+'; exact output is retained in '+$stderr)}
  return $record
}
try {
  # This lock excludes other copies of this runner, not uncoordinated development builds.
  $packageLock=[IO.FileStream]::new((Join-Path $packageWorkspace 'tests-artifacts/windows-production-package.capture.lock'),[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None,4096,[IO.FileOptions]::DeleteOnClose)
  $node=Resolve-PackageTool 'node.exe'
  $cargo=Resolve-PackageTool 'cargo.exe'
  $rustc=Resolve-PackageTool 'rustc.exe'
  $rustup=Resolve-PackageTool 'rustup.exe'
  $npmCommand=Get-Command 'npm.cmd' -CommandType Application -ErrorAction Stop | Select-Object -First 1
  $npmCli=Join-Path (Split-Path $npmCommand.Source) 'node_modules/npm/bin/npm-cli.js'
  if(-not (Test-Path -LiteralPath $npmCli -PathType Leaf)){throw 'Cannot qualify npm CLI adjacent to the installed npm.cmd'}
  $tauriCli=Join-Path $packageWorkspace 'node_modules/@tauri-apps/cli/tauri.js'
  if(-not (Test-Path -LiteralPath $packageCargoHome -PathType Container)){throw 'Workspace .cargo-home is unavailable; no download/fallback is permitted'}
  # Reject invisible feature/config/code/signing injection. Do not log environment values or secrets.
  $foreign=@(Get-ChildItem Env: | Where-Object {
    $_.Value -and (
      $_.Name -match '^(TAURI_|IMAGE_RANKER_|NSIS|RUSTFLAGS$|RUSTC$|RUSTDOC$|RUSTC_WRAPPER$|RUSTC_WORKSPACE_WRAPPER$|NODE_OPTIONS$|VITE_|npm_config_(?:script_shell|userconfig|globalconfig)$)' -or
      ($_.Name -match '^CARGO_' -and $_.Name -notin @('CARGO_HOME','CARGO_TARGET_DIR','CARGO_NET_OFFLINE'))
    )
  } | Select-Object -ExpandProperty Name)
  if($foreign.Count){throw ('Unreviewed build environment keys: '+($foreign -join ', '))}
  $null=Invoke-PackageCommand 'capture-inputs' $node @($packageHelper,'before',$packageWorkspace,$packageEvidence)
  $packageInputCaptured=$true
  $configCheck=Invoke-PackageCommand 'validate-package-config' $node @((Join-Path $PSScriptRoot 'validate-config.mjs'))
  $config=(Get-Content -LiteralPath $configCheck.stdout -Raw | ConvertFrom-Json).merged
  if($config.identifier -ne 'local.imageranker.desktop' -or $config.build.beforeBuildCommand -ne 'npm run build' -or $config.build.frontendDist -ne '../dist') {
    throw 'Unexpected production identity or frontend build path/command'
  }
  if($config.bundle.useLocalToolsDir -ne $true){throw 'NSIS tool capture requires the fresh target-local tool directory'}
  $packageJson=Get-Content -LiteralPath (Join-Path $packageWorkspace 'package.json') -Raw | ConvertFrom-Json
  if($packageJson.scripts.build -ne 'tsc --noEmit && vite build'){throw 'Frontend build command changed; review its production mode before qualifying'}
  $packageIdentityVerified=$true
  $null=Invoke-PackageCommand 'node-version' $node @('--version')
  $null=Invoke-PackageCommand 'npm-version' $node @($npmCli,'--version')
  $null=Invoke-PackageCommand 'npm-dependency-graph' $node @($npmCli,'ls','--all','--json','--offline')
  $rustVersion=Invoke-PackageCommand 'rustc-version' $rustc @('--version','--verbose')
  $null=Invoke-PackageCommand 'cargo-version' $cargo @('--version','--verbose')
  $null=Invoke-PackageCommand 'active-toolchain' $rustup @('show','active-toolchain')
  $null=Invoke-PackageCommand 'installed-targets' $rustup @('target','list','--installed')
  foreach($name in @('rustc','cargo')) {
    $resolved=Invoke-PackageCommand ('resolve-'+$name) $rustup @('which',$name)
    $toolPath=(Get-Content -LiteralPath $resolved.stdout -Raw).Trim()
    if(-not (Test-Path -LiteralPath $toolPath -PathType Leaf)){throw ('Qualified toolchain executable missing: '+$name)}
    $packageTools.Add([ordered]@{name='stable-'+$name;path=$toolPath;sha256=(Get-FileHash -LiteralPath $toolPath -Algorithm SHA256).Hash.ToLowerInvariant()})
  }
  $pin=[regex]::Match((Get-Content -LiteralPath (Join-Path $packageWorkspace 'rust-toolchain.toml') -Raw),'channel\s*=\s*"([0-9.]+)"').Groups[1].Value
  if(-not $pin -or (Get-Content -LiteralPath $rustVersion.stdout -Raw) -notmatch ('^rustc '+[regex]::Escape($pin)+' ')){throw 'The stable override does not match the project Rust version pin'}
  $null=Invoke-PackageCommand 'tauri-version' $node @($tauriCli,'--version')
  $help=Invoke-PackageCommand 'tauri-build-help' $node @($tauriCli,'build','--help')
  if((Get-Content -LiteralPath $help.stdout -Raw) -notmatch '--no-sign'){throw 'Installed CLI does not expose --no-sign; no substitute signing behavior was selected'}
  $metadataArgs=@('metadata','--manifest-path',$packageManifest,'--format-version','1','--locked','--offline','--no-default-features','--filter-platform','x86_64-pc-windows-msvc')
  $null=Invoke-PackageCommand 'requested-cargo-metadata' $cargo $metadataArgs
  $null=Invoke-PackageCommand 'requested-cargo-feature-tree' $cargo @('tree','--manifest-path',$packageManifest,'--locked','--offline','--no-default-features','--target','x86_64-pc-windows-msvc','--edges','features','--prefix','depth')
  $null=Invoke-PackageCommand 'notice-validation' $node @((Join-Path $packageWorkspace 'scripts/generate-notices.mjs'),'--check')
  # FIX 2: Cargo is offline/locked; Tauri's separate NSIS/WebView2 downloader is not claimed network-isolated.
  # --no-default-features is forwarded to Cargo. No --features/e2e or test-identity overlay is admitted.
  $packageStatus='building'
  Write-PackageJson (Join-Path $packageEvidence 'build-admission.json') ([ordered]@{startedAt=[DateTime]::UtcNow.ToString('o');identity='local.imageranker.desktop';target='x86_64-pc-windows-msvc';configuration='release/no-e2e/no-default-features/internal-unsigned';targetDirectory=$packageTarget;exclusiveWindowAsserted=$true})
  $build=Invoke-PackageCommand 'production-build' $node @($tauriCli,'build','--target','x86_64-pc-windows-msvc','--bundles','nsis','--config',$packageOverlay,'--no-sign','--ci','--verbose','--','--locked','--offline','--no-default-features','--verbose') -AllowFailure
  $packageBuildExit=$build.exitCode
  $packageStatus=if($packageBuildExit -eq 0){'build-returned-success'}else{'build-failed'}
} catch {
  $packageFailures.Add($_.Exception.Message)
  if($packageStatus -eq 'building'){$packageStatus='build-interrupted-or-launch-failed'}
} finally {
  # FIX 3: Failure still captures changed inputs and any actually produced bytes. Nothing is installed or signed here.
  if($packageInputCaptured) {
    try {
      $check=Invoke-PackageCommand 'verify-inputs' $node @($packageHelper,'after',$packageWorkspace,$packageEvidence) -AllowFailure
      if($check.exitCode -ne 0){$packageFailures.Add('Input verification failed; the build is not qualified')}
      if(Test-Path -LiteralPath (Join-Path $packageEvidence 'input-verification.json')){$packageVerification=Get-Content -LiteralPath (Join-Path $packageEvidence 'input-verification.json') -Raw | ConvertFrom-Json}
    } catch {$packageFailures.Add('Input verification: '+$_.Exception.Message)}
    try {$null=Invoke-PackageCommand 'capture-outputs' $node @($packageHelper,'outputs',$packageWorkspace,$packageEvidence,$packageTarget)} catch {$packageFailures.Add('Output capture: '+$_.Exception.Message)}
  }
  foreach($tool in $packageTools) {
    try {
      $tool.unchanged=(Get-FileHash -LiteralPath $tool.path -Algorithm SHA256).Hash.ToLowerInvariant() -eq $tool.sha256
      if(-not $tool.unchanged){$packageFailures.Add('Tool binary changed: '+$tool.name)}
    } catch {$packageFailures.Add('Tool verification: '+$_.Exception.Message)}
  }
  $artifacts=@();$signatureRecords=@();$featureEvidence=$null
  if(Test-Path -LiteralPath (Join-Path $packageEvidence 'outputs.json')) {
    $outputRecord=Get-Content -LiteralPath (Join-Path $packageEvidence 'outputs.json') -Raw | ConvertFrom-Json
    $artifacts=@($outputRecord.artifacts);$featureEvidence=$outputRecord.featureEvidence
    foreach($artifact in $artifacts) {
      try {
        $signature=Get-AuthenticodeSignature -LiteralPath $artifact.path
        $afterSignature=(Get-FileHash -LiteralPath $artifact.path -Algorithm SHA256).Hash.ToLowerInvariant()
        if($afterSignature -ne $artifact.sha256){throw 'Artifact changed during signature inspection'}
        $signatureRecords+=,[ordered]@{path=$artifact.path;sha256=$artifact.sha256;status=$signature.Status.ToString();statusMessage=$signature.StatusMessage;signerThumbprint=$signature.SignerCertificate.Thumbprint;timestampThumbprint=$signature.TimeStamperCertificate.Thumbprint;installed=$false;signedReleaseQualified=$false}
      } catch {
        $packageFailures.Add('Artifact/signature inspection: '+$_.Exception.Message)
        $signatureRecords+=,[ordered]@{path=$artifact.path;sha256=$artifact.sha256;status='inspection-failed';error=$_.Exception.Message;installed=$false;signedReleaseQualified=$false}
      }
    }
    if($packageBuildExit -eq 0 -and $null -ne $packageBuildExit) {
      if(@($artifacts | Where-Object kind -eq 'application').Count -ne 1 -or @($artifacts | Where-Object kind -eq 'nsis-installer').Count -ne 1){$packageFailures.Add('Successful CLI return did not produce exactly one app and one NSIS installer')}
      if(-not $featureEvidence){$packageFailures.Add('Actual production feature evidence is incomplete: '+$outputRecord.featureError)}
      if(@($outputRecord.generatedFrontend).Count -eq 0){$packageFailures.Add('Generated frontend output is absent')}
      foreach($issue in @($outputRecord.installerEvidence.issues)){$packageFailures.Add('Installer evidence: '+$issue)}
    }
  } elseif($packageBuildExit -eq 0 -and $null -ne $packageBuildExit){$packageFailures.Add('No output evidence was captured')}
  if($packageBuildExit -eq 0 -and $null -ne $packageBuildExit -and $packageFailures.Count -eq 0 -and $packageVerification.unchanged){$packageStatus='internal-package-produced-inputs-unchanged'}
  elseif($packageStatus -eq 'build-returned-success'){$packageStatus='build-produced-unqualified-evidence'}
  Write-PackageJson (Join-Path $packageEvidence 'capture.json') ([ordered]@{
    schemaVersion=1;startedAt=$packageStarted;finishedAt=[DateTime]::UtcNow.ToString('o');status=$packageStatus;workspace=$packageWorkspace;targetDirectory=$packageTarget;
    target='x86_64-pc-windows-msvc';normalProductionIdentityVerified=$packageIdentityVerified;internalUnsignedQualification=$true;installed=$false;signedReleaseQualified=$false;
    environment=[ordered]@{RUSTUP_TOOLCHAIN='stable';CARGO_HOME=$packageCargoHome;CARGO_TARGET_DIR=$packageTarget;CARGO_NET_OFFLINE='true';npm_config_offline='true'};
    networkQualification='Cargo/npm offline flags are enforced; separate Tauri bundler downloads are observable, not proven network-isolated. No installer execution or offline-installed launch was performed.';
    commands=@($packageCommands);tools=@($packageTools);buildExitCode=$packageBuildExit;inputVerification=$packageVerification;artifacts=$artifacts;authenticode=$signatureRecords;featureEvidence=$featureEvidence;installerEvidence=$outputRecord.installerEvidence;failures=@($packageFailures);
    limitations=@('Coordinator must exclude all other frontend/native builds; runner lock only excludes copies of this runner.','Logs and Cargo fingerprints are local qualification evidence and may contain local paths.','Artifact production is not installation, offline launch, upgrade, accessibility, performance, or signed-release qualification.','No automatic download repair, permission changes, process termination, output deletion, or reuse occurs.')
  })
  if($packageLock){$packageLock.Dispose()}
}
Write-Output ('Package capture: '+$packageStatus)
Write-Output ('Evidence: '+$packageEvidence)
if($packageStatus -ne 'internal-package-produced-inputs-unchanged'){exit 1}
