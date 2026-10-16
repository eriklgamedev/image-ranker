// Deterministic, reviewed downstream transformation. Does not build or execute NSIS.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const root=path.dirname(fileURLToPath(import.meta.url));
let text=fs.readFileSync(path.join(root,'upstream/installer.nsi'),'utf8').replaceAll('\r\n','\n');
function replace(from,to){if(!text.includes(from))throw new Error('Pinned installer seam changed: '+from.slice(0,80));text=text.replace(from,to);}
function cut(start,end,replacement=''){const a=text.indexOf(start),b=text.indexOf(end,a);if(a<0||b<0)throw new Error('Pinned range missing');text=text.slice(0,a)+replacement+text.slice(b);}
// FIX 1: All effect paths acquire native lifetime ownership, and no previous
// uninstaller is ever executed. The current v1 layout upgrades in place.
replace('!include "utils.nsh"\n','');replace('!include "Win\\RestartManager.nsh"\n','');
replace('{{#if installer_hooks}}\n!include "{{installer_hooks}}"\n{{/if}}','{{#if installer_hooks}}\n!include "{{installer_hooks}}"\n{{else}}\n!error "Image Ranker native lifetime include is required"\n{{/if}}');
replace('Var WixMode\nVar OldMainBinaryName\n','');
cut('; 4. Custom page to ask user if he wants to reinstall/uninstall','; 6. Start menu shortcut page','; No maintenance-uninstaller or arbitrary install-directory page.\n');
cut('; 6. Start menu shortcut page','; 7. Installation page','Var AppStartMenuFolder\n');
cut('; Use show readme button','!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive','; Launch manually after the installer closes. No effects follow release.\n');
cut('Function RunMainBinary','; Uninstaller Pages');
cut('Var DeleteAppDataCheckbox','!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive','!define MUI_UNCONFIRMPAGE_TEXT_TOP "Only the installed application and its registration will be removed. Images, saved rankings, cache and recovery data are preserved."\n');
const begin=text.indexOf('Function .onInit'),end=text.indexOf('FunctionEnd',begin)+'FunctionEnd'.length;
text=text.slice(0,begin)+`Function .onInit
  Push "${'${VERSION}'}"
  ImageRankerLifetime::AcquireInstall
  !insertmacro ImageRankerRequireSuccess
  ClearErrors
  Pop $INSTDIR
  IfErrors 0 +2
    Abort "Native install location was not returned."
  !insertmacro SetContext
  StrCpy $AppStartMenuFolder "Image Ranker"
  ${'${GetOptions}'} $CMDLINE "/P" $PassiveMode
  ${'${IfNot}'} ${'${Errors}'}
    StrCpy $PassiveMode 1
  ${'${EndIf}'}
  ${'${GetOptions}'} $CMDLINE "/NS" $NoShortcutMode
  ${'${IfNot}'} ${'${Errors}'}
    StrCpy $NoShortcutMode 1
  ${'${EndIf}'}
FunctionEnd`+text.slice(end);
cut('Section EarlyChecks','Section WebView2','; Downgrade and predecessor identity checks are native admission.\n');
replace('Section WebView2\n','Section WebView2\n  !insertmacro ImageRankerValidate\n');
replace('Section Install\n  SetOutPath $INSTDIR','Section Install\n  !insertmacro ImageRankerValidate\n  ImageRankerLifetime::BeginInstall\n  !insertmacro ImageRankerRequireSuccess\n  SetOutPath $INSTDIR');
replace('  !insertmacro CheckIfAppIsRunning "$INSTDIR\\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"','  !insertmacro ImageRankerValidate');
// FIX 2: Fixed two-file package ownership; expanding resources requires a new
// native layout protocol, never accepting an unreviewed loop of path authority.
cut('  ; Copy resources','  ; Create uninstaller');
replace('  WriteUninstaller "$INSTDIR\\uninstall.exe"','  WriteUninstaller "$INSTDIR\\uninstall.exe"\n  IfErrors 0 +2\n    Abort "Writing package files failed; saved data has been preserved."\n  ImageRankerLifetime::CompleteInstall\n  !insertmacro ImageRankerRequireSuccess');
replace('  !insertmacro MUI_STARTMENU_WRITE_BEGIN Application\n    Call CreateOrUpdateStartMenuShortcut\n  !insertmacro MUI_STARTMENU_WRITE_END','  Call CreateOrUpdateStartMenuShortcut');
replace('    !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder','    StrCpy $AppStartMenuFolder "Image Ranker"');
cut('  ; Remove old main binary if it doesn\'t match new main binary name','  ; Save current MAINBINARYNAME');
cut('Function .onInstSuccess','Function un.onInit');
replace('Function un.onInit\n','Function un.onInit\n  ImageRankerLifetime::AcquireUninstall\n  !insertmacro ImageRankerRequireSuccess\n  ClearErrors\n  Pop $INSTDIR\n  IfErrors 0 +2\n    Abort "Native uninstall location was not returned."\n  StrCpy $AppStartMenuFolder "Image Ranker"\n');
replace('Section Uninstall\n','Section Uninstall\n  !insertmacro ImageRankerValidate\n');
replace('  !insertmacro CheckIfAppIsRunning "$INSTDIR\\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"','  !insertmacro ImageRankerValidate');
cut('  ; Delete the app directory and its content from disk','  ; Remove shortcuts if not updating','  ; Delete only the exact native-enrolled file handles. The directory and unknown files remain.\n  ImageRankerLifetime::RemoveOwnedFiles\n  !insertmacro ImageRankerRequireSuccess\n\n');
cut('  ; Delete app data if the checkbox is selected','  !ifmacrodef NSIS_HOOK_POSTUNINSTALL','  ; Preserved data/launcher/gate have no uninstall deletion path.\n\n');
cut('Function RestorePreviousInstallLocation','Function Skip\n');
const shortcuts=text.indexOf('Function CreateOrUpdateStartMenuShortcut');
if(shortcuts<0)throw new Error('Shortcut seam missing');
text=text.slice(0,shortcuts)+`Function CreateOrUpdateStartMenuShortcut
  !insertmacro ImageRankerValidate
  ${'${If}'} $NoShortcutMode != 1
    CreateDirectory "$SMPROGRAMS\\Image Ranker"
    IfFileExists "$SMPROGRAMS\\Image Ranker\\Image Ranker.lnk" done_start
    CreateShortcut "$SMPROGRAMS\\Image Ranker\\Image Ranker.lnk" "$INSTDIR\\image-ranker.exe"
  ${'${EndIf}'}
  done_start:
FunctionEnd
Function CreateOrUpdateDesktopShortcut
  !insertmacro ImageRankerValidate
  ${'${If}'} $NoShortcutMode != 1
    IfFileExists "$DESKTOP\\Image Ranker.lnk" done_desktop
    CreateShortcut "$DESKTOP\\Image Ranker.lnk" "$INSTDIR\\image-ranker.exe"
  ${'${EndIf}'}
  done_desktop:
FunctionEnd
`;
// FIX 3: There are no finish-page mutations or automatic app launches. Keep the
// owner through both whole sections, releasing only after their final effects.
replace('  ; Auto close this page for passive mode','  ImageRankerLifetime::ReleaseAfterEffects\n  !insertmacro ImageRankerRequireSuccess\n\n  ; Auto close this page for passive mode');
replace('  ; Auto close if passive mode or updating','  ImageRankerLifetime::ReleaseAfterEffects\n  !insertmacro ImageRankerRequireSuccess\n\n  ; Auto close if passive mode or updating');
const guards=`
!if "${'${INSTALLMODE}'}" != "currentUser"
  !error "Only current-user package ownership is supported"
!endif
!if "${'${ARCH}'}" != "x64"
  !error "The supported application architecture is Windows x64"
!endif
!if "${'${BUNDLEID}'}" != "local.imageranker.desktop"
  !error "This template requires the production Image Ranker identity"
!endif
!if "${'${MAINBINARYNAME}'}" != "image-ranker"
  !error "The fixed owned package layout changed"
!endif
{{#each resources}}!error "Resources require a new qualified native package layout"{{/each}}
{{#each binaries}}!error "External binaries require a new qualified native package layout"{{/each}}
{{#each file_associations}}!error "Associations require separate qualification"{{/each}}
{{#each deep_link_protocols}}!error "Deep links require separate qualification"{{/each}}
`;
replace('Var PassiveMode',guards+'\nVar PassiveMode');
text='; Downstream Image Ranker preservation/lifetime changes; pinned Tauri CLI2.12.0 original is retained in upstream/.\n'+text;
fs.writeFileSync(path.join(root,'installer.nsi'),text);
let utils=fs.readFileSync(path.join(root,'upstream/utils.nsh'),'utf8').replaceAll('\r\n','\n');
const a=utils.indexOf('; Checks whether app is running'),b=utils.indexOf('; Sets AppUserModelId',a);if(a<0||b<0)throw new Error('Pinned force macro seam missing');
utils=utils.slice(0,a)+'; Running application exclusion is held natively; no process shutdown macro.\n\n'+utils.slice(b);
fs.writeFileSync(path.join(root,'image-ranker-utils.nsh'),utils);
