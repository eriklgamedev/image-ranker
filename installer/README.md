# Installer source checkpoint

This source is **not compiled, installed, signed, or qualified**. The application
and decoder have not yet been integrated with the shared lease. Do not treat a
template build alone as a safe deployable installer.

The source design now uses an in-process Unicode x86 NSIS plugin and the separate
Windows `src-tauri/install-lifetime-core` crate. Package installation is restricted
to the fixed current-user `LocalAppData/Image Ranker` two-file v1 layout. An
enrollment record binds the actual directory identity, numeric version and the
app/uninstaller identities and SHA-256. Unknown prior targets and downgrades fail
closed. Upgrades happen in place under the same exclusive lease; no prior
uninstaller is executed, so no nested-child lease handoff is required by this
template. Standalone uninstall obtains its own exclusive lease.

The original CLI v2.12.0 template and utilities and their Apache/MIT notices are
preserved under `nsis/upstream`. The downstream transformation is explicitly marked
and reproducible by `nsis/generate-template.mjs`. NSIS v3.11 ABI references and
their original license are under `nsis-plugin/upstream`; their source commit is
recorded in its manifest. These are references, not proof of the actual bundled
compiler/stub/plugin ABI.

Only the enrolled two files are candidates for removal. User images, ranking
profiles, recovery copies, cache, unknown files, launcher state and the gate have
no uninstall deletion path. The target directory remains. Automatic launch and
finish-page shortcut creation are removed: launch manually after installer exit.
This keeps the last installer effect before lease release.

Before runtime qualification, the coordinator must:

1. Review and compile the core, resolve the separate locked dependency graphs and
   run native contention/security/identity tests. Native source tests here have
   not run. No compile success is implied by the JavaScript static checks.
2. Integrate the shared app lease before all store access and retain it until
   process exit. Add authenticated exact duplicate and route-pin transfer to the
   decoder's existing explicit native handle protocol. No raw handle-number
   adoption API has been added to this crate.
3. Build the plugin for `i686-pc-windows-msvc`, capture its toolchain and source
   graph, and stage the verified `ImageRankerLifetime.dll` in
   `nsis-plugin/artifacts`. The coordinator provisioned the i686 compiler target;
   no plugin build has run yet. The plugin pins its own module until exit.
4. Review native package preparation failure recovery, NSIS overwrite sharing,
   uninstaller temporary re-exec, enrollment publication and exact held deletion
   behavior. Test the real first-install, A→B update, downgrade refusal,
   standalone uninstall and interrupted-effects paths with originals preserved.
5. Capture every generated installer/include/tool/plugin input and exercise
   standard-user busy/quiet/passive/offline/upgrade/uninstall scenarios. Public
   release still needs signing, licensing review and the other release gates.

Wizard cancellation before the install section leaves no executable placeholders.
Enrollment updates write the exact qualified held record rather than replacing a
path occupant. Interrupted package/enrollment writes are not transactional rollback
and can require explicit support recovery; no automatic destructive repair exists.
Qualification failures or
foreign changes must remain explicit; no process kill, permission change, raw
registry-command execution, fallback gate, or destructive recovery is available.
