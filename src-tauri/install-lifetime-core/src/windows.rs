use crate::{harmful_grant, harmful_route_grant, trusted_sid, trusted_route_owner, Error, Mode, Result};
use std::{ffi::{c_void, OsStr, OsString}, fs::File, mem::{size_of, zeroed}, os::windows::{ffi::{OsStrExt, OsStringExt}, io::{AsRawHandle, FromRawHandle}}, path::{Component, Path, PathBuf}, ptr};
use windows_sys::Win32::{Foundation::*, Security::{Authorization::*, *}, Storage::FileSystem::*, System::{Com::CoTaskMemFree, Threading::*}, UI::Shell::{SHGetKnownFolderPath, FOLDERID_LocalAppData}};
mod package;
pub use package::PackageGuard;

const LAUNCHER: &str = "ImageRankerLauncher";
const GATE: &str = "install-lifetime-v1.lock";
const MAX_ROUTE: usize = 64;
// GetDriveTypeW's documented DRIVE_FIXED result (WindowsProgramming constant).
const DRIVE_FIXED: u32 = 3;
fn reject(stage: &'static str) -> Error { Error { code: "INSTALLER_INTERLOCK_UNAVAILABLE", stage, os_code: None } }
fn os_error(stage: &'static str, code: u32) -> Error { Error { code: if code == ERROR_SHARING_VIOLATION || code == ERROR_LOCK_VIOLATION { "INSTALLER_APP_BUSY" } else { "INSTALLER_INTERLOCK_UNAVAILABLE" }, stage, os_code: Some(code) } }
fn last_error(stage: &'static str) -> Error { os_error(stage, unsafe { GetLastError() }) }
fn wide(value: &OsStr) -> Result<Vec<u16>> { let mut v: Vec<u16> = value.encode_wide().collect(); if v.is_empty() || v.len() > 32760 || v.contains(&0) { return Err(reject("invalid native path")); } v.push(0); Ok(v) }

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity { volume: u32, high: u32, low: u32 }
struct Pin { file: File, id: Identity, name: OsString, owned: bool }
pub struct Lease { mode: Mode, sid: String, pins: Vec<Pin>, gate: File, gate_id: Identity, launcher: PathBuf }

impl Lease {
    pub fn mode(&self) -> Mode { self.mode }
    /// Display/integration location only; the held native objects supply authority.
    pub fn launcher_path(&self) -> &Path { &self.launcher }
    pub fn validate(&self) -> Result<()> {
        if current_sid()? != self.sid { return Err(reject("process account changed")); }
        for (index, pin) in self.pins.iter().enumerate() {
            if identity(&pin.file, true)? != pin.id { return Err(reject("held directory identity changed")); }
            security_policy(&pin.file, &self.sid, pin.owned)?;
            if index > 0 {
                let linked = relative(&self.pins[index-1].file, &pin.name, true, false, FILE_READ_ATTRIBUTES | READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE, ptr::null_mut())?;
                if identity(&linked, true)? != pin.id { return Err(reject("held directory route changed")); }
            }
        }
        qualify_gate(&self.gate, &self.sid)?;
        if identity(&self.gate, false)? != self.gate_id { return Err(reject("held gate identity changed")); }
        let rebound = relative(&self.pins.last().ok_or_else(|| reject("missing launcher pin"))?.file, OsStr::new(GATE), false, false, FILE_READ_ATTRIBUTES | READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE, ptr::null_mut())?;
        if identity(&rebound, false)? != self.gate_id { return Err(reject("gate binding changed")); }
        Ok(())
    }
    /// Keep the shared app owner until OS teardown, including native background
    /// work after a UI closes. There is deliberately no renderer release route.
    pub fn retain_until_process_exit(self) -> &'static Self { Box::leak(Box::new(self)) }
}

pub fn acquire_shared() -> Result<Lease> { acquire(Mode::SharedApplication) }
pub fn acquire_exclusive() -> Result<Lease> { acquire(Mode::ExclusiveInstaller) }

fn acquire(mode: Mode) -> Result<Lease> {
    let sid = current_sid()?;
    let (local,mut pins)=known_folder_route(&sid)?;
    let descriptor = security_descriptor(&sid)?;
    let file = relative(&pins.last().unwrap().file, OsStr::new(LAUNCHER), true, true, FILE_READ_ATTRIBUTES | READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE, descriptor.0)?;
    security(&file, &sid)?;
    pins.push(Pin { id: identity(&file, true)?, file, name: OsString::from(LAUNCHER), owned:true });
    let share = match mode { Mode::SharedApplication => FILE_SHARE_READ, Mode::ExclusiveInstaller => 0 };
    let gate = relative(&pins.last().unwrap().file, OsStr::new(GATE), false, true, FILE_GENERIC_READ, share, descriptor.0)?;
    qualify_gate(&gate, &sid)?;
    let gate_id = identity(&gate, false)?;
    let lease = Lease { mode, sid, pins, gate, gate_id, launcher: local.join(LAUNCHER) };
    lease.validate()?;
    Ok(lease)
}
fn known_folder_route(sid:&str)->Result<(PathBuf,Vec<Pin>)> {
    let local = known_folder()?;
    let components: Vec<_> = local.components().collect();
    if components.len() < 3 || components.len() > MAX_ROUTE { return Err(reject("unsupported known-folder route")); }
    let drive = match components[0] { Component::Prefix(prefix) => match prefix.kind() { std::path::Prefix::Disk(drive) => drive, _ => return Err(reject("known-folder must use a local drive")) }, _ => return Err(reject("known-folder must be absolute")) };
    if !matches!(components[1], Component::RootDir) { return Err(reject("known-folder must be rooted")); }
    let root = PathBuf::from(format!("{}:\\", drive as char));
    let root_wide = wide(root.as_os_str())?;
    if unsafe { GetDriveTypeW(root_wide.as_ptr()) } != DRIVE_FIXED { return Err(reject("known-folder is not on fixed local storage")); }
    let root_handle = unsafe { CreateFileW(root_wide.as_ptr(), FILE_READ_ATTRIBUTES | READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE, ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, ptr::null_mut()) };
    if root_handle == INVALID_HANDLE_VALUE { return Err(last_error("pin drive root")); }
    let root_file = unsafe { File::from_raw_handle(root_handle) };
    let mut pins = vec![Pin { id: identity(&root_file, true)?, file: root_file, name: root.as_os_str().to_owned(), owned:false }];
    security_policy(&pins[0].file, sid, false)?;
    for (index,component) in components.iter().enumerate().skip(2) {
        let name = match component { Component::Normal(name) => *name, _ => return Err(reject("unqualified known-folder component")) };
        let file = relative(&pins.last().unwrap().file, name, true, false, FILE_READ_ATTRIBUTES | READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE, ptr::null_mut())?;
        let owned=index==components.len()-1;
        security_policy(&file, sid, owned)?;
        pins.push(Pin { id: identity(&file, true)?, file, name: name.to_owned(), owned });
    }
    Ok((local,pins))
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation { fn drop(&mut self) { if !self.0.is_null() { unsafe { LocalFree(self.0); } } } }
fn sid_string(sid: PSID) -> Result<String> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 { return Err(reject("invalid account SID")); }
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 { return Err(last_error("format account SID")); }
    let allocation = LocalAllocation(text.cast());
    let mut len=0; while len<256 && unsafe { *text.add(len) } != 0 { len+=1; }
    if len==256 { return Err(reject("account SID exceeds bound")); }
    let value=String::from_utf16(unsafe { std::slice::from_raw_parts(text,len) }).map_err(|_| reject("invalid account SID encoding"))?;
    drop(allocation);Ok(value)
}
fn current_sid() -> Result<String> {
    let mut thread_token=ptr::null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread_token) } != 0 {
        unsafe { CloseHandle(thread_token); } return Err(reject("impersonated thread is unsupported"));
    }
    if unsafe { GetLastError() } != ERROR_NO_TOKEN { return Err(last_error("query thread identity")); }
    let mut handle=ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 { return Err(last_error("open process identity")); }
    let token=unsafe { File::from_raw_handle(handle) };let mut required=0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, ptr::null_mut(),0,&mut required); }
    if required < size_of::<TOKEN_USER>() as u32 || required>65536 { return Err(reject("unexpected process token size")); }
    let mut buffer=vec![0u64; (required as usize+7)/8];
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser,buffer.as_mut_ptr().cast(),required,&mut required) } == 0 { return Err(last_error("read process identity")); }
    sid_string(unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid })
}
fn known_folder() -> Result<PathBuf> {
    let mut path=ptr::null_mut();
    let result=unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData,0,ptr::null_mut(),&mut path) };
    if result<0 || path.is_null() { return Err(reject("resolve native LocalAppData")); }
    let mut len=0;while len<32760 && unsafe { *path.add(len) }!=0 {len+=1;}
    let value=if len==32760 { Err(reject("known-folder path exceeds bound")) } else { Ok(PathBuf::from(OsString::from_wide(unsafe {std::slice::from_raw_parts(path,len)}))) };
    unsafe { CoTaskMemFree(path.cast()); } value
}
fn security_descriptor(sid: &str) -> Result<LocalAllocation> {
    let sddl=wide(OsStr::new(&format!("O:{sid}G:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})")))?;
    let mut descriptor=ptr::null_mut();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),1,&mut descriptor,ptr::null_mut()) }==0 { return Err(last_error("create private gate descriptor")); }
    Ok(LocalAllocation(descriptor))
}
fn security(file: &File, current: &str) -> Result<()> {
    security_policy(file,current,true)
}
fn security_policy(file: &File, current: &str, owned:bool) -> Result<()> {
    let mut owner=ptr::null_mut();let mut acl=ptr::null_mut();let mut descriptor=ptr::null_mut();
    let status=unsafe { GetSecurityInfo(file.as_raw_handle(),SE_FILE_OBJECT,OWNER_SECURITY_INFORMATION|DACL_SECURITY_INFORMATION,&mut owner,ptr::null_mut(),&mut acl,ptr::null_mut(),&mut descriptor) };
    if status!=0 { return Err(os_error("inspect held object security",status)); }
    let _allocation=LocalAllocation(descriptor);
    let owner=sid_string(owner)?;
    if !(if owned{trusted_sid(&owner,current)}else{trusted_route_owner(&owner,current)}) || acl.is_null() || unsafe { IsValidAcl(acl) }==0 { return Err(reject("object has unqualified owner or DACL")); }
    let count=unsafe { (*acl).AceCount };if count>128 {return Err(reject("object DACL exceeds bound"));}
    for index in 0..count {
        let mut ace=ptr::null_mut();if unsafe { GetAce(acl,index as u32,&mut ace) }==0 {return Err(last_error("read object ACE"));}
        let header=unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceFlags & INHERIT_ONLY_ACE as u8 != 0 {continue;}
        match header.AceType {
            0 => {
                if (header.AceSize as usize)<size_of::<ACCESS_ALLOWED_ACE>() {return Err(reject("truncated allow ACE"));}
                let allowed=unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid_bytes=(header.AceSize as usize).checked_sub(8).ok_or_else(||reject("truncated ACE SID"))?;
                if sid_bytes<8 {return Err(reject("truncated ACE SID"));}
                let sid=ptr::addr_of!(allowed.SidStart).cast_mut().cast::<c_void>();
                let subauthorities=unsafe {*(sid.cast::<u8>().add(1))} as usize;
                if subauthorities>15 || 8+subauthorities*4>sid_bytes {return Err(reject("ACE SID exceeds its record"));}
                let grantee=sid_string(sid)?;
                if (if owned{harmful_grant(allowed.Mask)}else{harmful_route_grant(allowed.Mask)}) && !(if owned{trusted_sid(&grantee,current)}else{trusted_route_owner(&grantee,current)}) { return Err(reject("foreign modifying DACL grant")); }
            },
            1 => {}, // Denies cannot increase the conservative allowed rights.
            _ => return Err(reject("unsupported object ACE type")),
        }
    }
    Ok(())
}
fn identity(file: &File, directory: bool) -> Result<Identity> {
    let mut info:BY_HANDLE_FILE_INFORMATION=unsafe {zeroed()};
    if unsafe {GetFileInformationByHandle(file.as_raw_handle(),&mut info)}==0 {return Err(last_error("inspect held object"));}
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT !=0 || (info.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY!=0)!=directory {return Err(reject("unexpected object type or reparse point"));}
    if !directory && info.nNumberOfLinks!=1 {return Err(reject("file must be single-link"));}
    Ok(Identity {volume:info.dwVolumeSerialNumber,high:info.nFileIndexHigh,low:info.nFileIndexLow})
}
fn qualify_gate(file: &File, sid: &str) -> Result<()> {
    identity(file,false)?;security(file,sid)?;
    if file.metadata().map_err(|_|reject("query gate size"))?.len()!=0 {return Err(reject("gate must be empty"));}
    let mut fs_name=[0u16;32];
    if unsafe {GetVolumeInformationByHandleW(file.as_raw_handle(),ptr::null_mut(),0,ptr::null_mut(),ptr::null_mut(),ptr::null_mut(),fs_name.as_mut_ptr(),fs_name.len() as u32)}==0 {return Err(last_error("inspect gate filesystem"));}
    let end=fs_name.iter().position(|n|*n==0).ok_or_else(||reject("filesystem name exceeds bound"))?;
    if String::from_utf16_lossy(&fs_name[..end])!="NTFS" {return Err(reject("gate filesystem is not NTFS"));}
    // FileStreamInformation is queried on the held object, not a pathname.
    let mut buffer=[0u64;1024];let mut status=IoStatusBlock {status:0,information:0};
    let code=unsafe {NtQueryInformationFile(file.as_raw_handle(),&mut status,buffer.as_mut_ptr().cast(),8192,22)};
    if code<0 {return Err(os_error("inspect gate streams",unsafe {RtlNtStatusToDosError(code)}));}
    if status.information<24 || status.information>8192 {return Err(reject("invalid gate stream inventory"));}
    let bytes=unsafe {std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(),status.information)};
    let next=u32::from_ne_bytes(bytes[0..4].try_into().unwrap());let length=u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if next!=0 || length!=14 || status.information<24+length || bytes[8..16] != [0;8] {return Err(reject("gate has unexpected streams"));}
    let name:Vec<u16>=bytes[24..24+length].chunks_exact(2).map(|b|u16::from_ne_bytes([b[0],b[1]])).collect();
    if String::from_utf16_lossy(&name)!="::$DATA" {return Err(reject("gate has alternate data stream"));}
    Ok(())
}

#[repr(C)] struct UnicodeString {length:u16,maximum_length:u16,buffer:*mut u16}
#[repr(C)] struct ObjectAttributes {length:u32,root:HANDLE,name:*mut UnicodeString,attributes:u32,security_descriptor:*mut c_void,security_qos:*mut c_void}
#[repr(C)] struct IoStatusBlock {status:usize,information:usize}
#[link(name="ntdll")] extern "system" {
    fn NtCreateFile(handle:*mut HANDLE,access:u32,attributes:*mut ObjectAttributes,status:*mut IoStatusBlock,allocation:*const i64,file_attributes:u32,share:u32,disposition:u32,options:u32,ea:*const c_void,ea_length:u32)->i32;
    fn NtQueryInformationFile(handle:HANDLE,status:*mut IoStatusBlock,information:*mut c_void,length:u32,class:u32)->i32;
    fn RtlNtStatusToDosError(status:i32)->u32;
}
fn relative(parent:&File,name:&OsStr,directory:bool,create:bool,access:u32,share:u32,descriptor:*mut c_void)->Result<File>{
    let mut name:Vec<u16>=name.encode_wide().collect();
    if name.is_empty()||name.len()>255||name.iter().any(|c|matches!(*c,0|47|92|58))||name==[46]||name==[46,46]{return Err(reject("invalid relative component"));}
    let mut unicode=UnicodeString {length:(name.len()*2) as u16,maximum_length:(name.len()*2) as u16,buffer:name.as_mut_ptr()};
    let mut attributes=ObjectAttributes {length:size_of::<ObjectAttributes>() as u32,root:parent.as_raw_handle(),name:&mut unicode,attributes:0x40,security_descriptor:descriptor,security_qos:ptr::null_mut()};
    let mut status=IoStatusBlock {status:0,information:0};let mut handle=ptr::null_mut();
    // OPEN_IF creates only an absent object; it never truncates an existing one.
    // OBJ_INHERIT is absent, and every operation stays relative to a held parent.
    let code=unsafe {NtCreateFile(&mut handle,access|SYNCHRONIZE,&mut attributes,&mut status,ptr::null(),FILE_ATTRIBUTE_NORMAL,share,if create {3}else{1},0x00200000|0x20|if directory {1}else{0x40},ptr::null(),0)};
    if code<0 {return Err(os_error("open held relative object",unsafe {RtlNtStatusToDosError(code)}));}
    Ok(unsafe {File::from_raw_handle(handle)})
}

#[cfg(test)]
mod tests {
    use super::*;
    // These native handle tests intentionally need Windows and normal filesystem
    // permissions. They are source fixtures until the coordinator executes them.
    fn fixture() -> PathBuf {
        let path=std::env::temp_dir().join(format!("image-ranker-lease-test-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&path).unwrap();path
    }
    fn directory(path:&Path)->File {
        let text=wide(path.as_os_str()).unwrap();let handle=unsafe {CreateFileW(text.as_ptr(),FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,ptr::null(),OPEN_EXISTING,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT,ptr::null_mut())};
        assert_ne!(handle,INVALID_HANDLE_VALUE);unsafe {File::from_raw_handle(handle)}
    }
    #[test]
    fn actual_native_known_folder_route_qualifies_without_creating_a_gate() {
        let sid=current_sid().unwrap();let (path,pins)=known_folder_route(&sid).unwrap();
        assert_eq!(path,known_folder().unwrap());assert!(!pins.is_empty());assert!(pins.last().unwrap().owned);
        for pin in pins {assert_eq!(identity(&pin.file,true).unwrap(),pin.id);security_policy(&pin.file,&sid,pin.owned).unwrap();}
    }
    #[test]
    fn actual_shared_readers_exclude_exclusive_until_last_owner_drops() {
        let path=fixture();let parent=directory(&path);let sid=current_sid().unwrap();let descriptor=security_descriptor(&sid).unwrap();
        let open=|share|relative(&parent,OsStr::new(GATE),false,true,FILE_GENERIC_READ,share,descriptor.0);
        let first=open(FILE_SHARE_READ).unwrap();let second=open(FILE_SHARE_READ).unwrap();
        assert!(matches!(open(0),Err(Error {code:"INSTALLER_APP_BUSY",..})));
        drop(first);assert!(open(0).is_err());drop(second);
        let exclusive=open(0).unwrap();assert!(open(FILE_SHARE_READ).is_err());drop(exclusive);drop(parent);
        std::fs::remove_file(path.join(GATE)).unwrap();std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn held_gate_cannot_be_replaced_and_nonempty_existing_gate_is_preserved() {
        let path=fixture();let parent=directory(&path);let sid=current_sid().unwrap();let descriptor=security_descriptor(&sid).unwrap();
        std::fs::write(path.join(GATE),b"foreign").unwrap();
        let held=relative(&parent,OsStr::new(GATE),false,true,FILE_GENERIC_READ,FILE_SHARE_READ,descriptor.0).unwrap();
        assert!(qualify_gate(&held,&sid).is_err());assert!(std::fs::remove_file(path.join(GATE)).is_err());
        assert_eq!(std::fs::read(path.join(GATE)).unwrap(),b"foreign");drop(held);drop(parent);
        std::fs::remove_file(path.join(GATE)).unwrap();std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn relative_object_open_cannot_escape_pinned_parent() {
        let path=fixture();let parent=directory(&path);
        for name in ["..","a\\b","a/b","file:stream","."] {assert!(relative(&parent,OsStr::new(name),false,true,FILE_GENERIC_READ,0,ptr::null_mut()).is_err());}
        drop(parent);std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn pinned_route_denies_replacement_and_delete_until_owner_exits() {
        let path=fixture();let parent=directory(&path);let child=path.join("bound");std::fs::create_dir(&child).unwrap();
        let held=relative(&parent,OsStr::new("bound"),true,false,FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,ptr::null_mut()).unwrap();
        let before=identity(&held,true).unwrap();
        assert!(std::fs::rename(&child,path.join("replacement")).is_err());assert!(std::fs::remove_dir(&child).is_err());
        let again=relative(&parent,OsStr::new("bound"),true,false,FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,ptr::null_mut()).unwrap();
        assert_eq!(identity(&again,true).unwrap(),before);drop(again);drop(held);drop(parent);
        std::fs::remove_dir(child).unwrap();std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn actual_foreign_acl_grants_distinguish_sibling_creation_from_route_mutation() {
        let path=fixture();let parent=directory(&path);let sid=current_sid().unwrap();
        // Every descriptor is confined to a newly created test directory. No
        // host KnownFolder/launcher ACL is changed by this fixture.
        for(mask,ancestor_ok)in [(4u32,true),(2,true),(0x40,false),(0x10000,false),(0x40000,false),(0x80000,false),(0x100,false),(0x40000000,false)]{
            let name=format!("grant-{mask}");
            let text=wide(OsStr::new(&format!("O:{sid}G:{sid}D:P(A;;FA;;;{sid})(A;;0x{mask:x};;;BU)"))).unwrap();let mut raw=ptr::null_mut();
            assert_ne!(unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(),1,&mut raw,ptr::null_mut())},0);let descriptor=LocalAllocation(raw);
            let held=relative(&parent,OsStr::new(&name),true,true,FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,descriptor.0).unwrap();
            assert_eq!(security_policy(&held,&sid,false).is_ok(),ancestor_ok,"mask {mask:x}");
            assert!(security_policy(&held,&sid,true).is_err(),"owned route mask {mask:x}");
            drop(held);std::fs::remove_dir(path.join(name)).unwrap();
        }
        drop(parent);std::fs::remove_dir(path).unwrap();
    }
}
