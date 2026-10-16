//! Fixed v1 package only. No arbitrary registry command, resource set, or child
//! uninstaller is admitted. Native identities and bytes qualify each predecessor.
use super::*;
use std::io::{Read, Seek, SeekFrom, Write};
use sha2::{Digest, Sha256};
const RECORD: &str = "install-layout-v1.json";
const TARGET: &str = "Image Ranker";
const FILES: [&str;2] = ["image-ranker.exe", "uninstall.exe"];
const MAX_FILE:u64=1024*1024*1024;

#[derive(serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Entry {name:String,identity:Identity,bytes:u64,sha256:String}
#[derive(serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Enrollment {schema_version:u32,product:String,target:Identity,version:[u32;3],installed:bool,files:Vec<Entry>}
pub struct PackageGuard {lease:Lease,target:File,target_id:Identity,path:PathBuf,version:[u32;3],files:Vec<(String,File,Identity)>,record:Option<File>,uninstall:bool,started:bool,finished:bool}

fn version(value:&str)->Result<[u32;3]>{
    let parts:Vec<_>=value.split('.').collect();if parts.len()!=3{return Err(reject("only numeric v1 package versions are supported"));}
    let mut parsed=[0;3];for(index,part)in parts.iter().enumerate(){if part.is_empty()||part.len()>10||!part.bytes().all(|c|c.is_ascii_digit()){return Err(reject("invalid package version"));}parsed[index]=part.parse().map_err(|_|reject("invalid package version"))?;}Ok(parsed)
}
fn read_hash(file:&File)->Result<(u64,String)>{
    let before=file.metadata().map_err(|_|reject("read package size"))?;if before.len()>MAX_FILE{return Err(reject("package artifact exceeds bound"));}
    let mut reader=file.try_clone().map_err(|_|reject("clone held package file"))?;reader.seek(SeekFrom::Start(0)).map_err(|_|reject("seek package file"))?;
    let mut hash=Sha256::new();let mut buffer=[0u8;65536];let mut total=0;
    loop{let n=reader.read(&mut buffer).map_err(|_|reject("read held package bytes"))?;if n==0{break;}total+=n as u64;if total>MAX_FILE{return Err(reject("package artifact changed beyond bound"));}hash.update(&buffer[..n]);}
    if total!=before.len()||file.metadata().map_err(|_|reject("verify package size"))?.len()!=total{return Err(reject("package artifact changed during verification"));}
    Ok((total,format!("{:x}",hash.finalize())))
}
fn load_record(lease:&Lease)->Result<Option<(Enrollment,File)>>{
    let result=relative(&lease.pins.last().unwrap().file,OsStr::new(RECORD),false,false,FILE_GENERIC_READ|FILE_GENERIC_WRITE,FILE_SHARE_READ,ptr::null_mut());
    let file=match result{Ok(file)=>file,Err(Error{os_code:Some(ERROR_FILE_NOT_FOUND),..})=>return Ok(None),Err(error)=>return Err(error)};
    identity(&file,false)?;security(&file,&lease.sid)?;
    let size=file.metadata().map_err(|_|reject("read enrollment size"))?.len();if size==0||size>65536{return Err(reject("invalid enrollment size"));}
    let mut bytes=Vec::with_capacity(size as usize);(&file).take(65537).read_to_end(&mut bytes).map_err(|_|reject("read enrollment"))?;
    if bytes.len()as u64!=size{return Err(reject("enrollment changed while reading"));}
    let record:Enrollment=serde_json::from_slice(&bytes).map_err(|_|reject("invalid package enrollment"))?;
    if record.schema_version!=1||record.product!="local.imageranker.desktop"||record.files.len()>2{return Err(reject("unsupported package enrollment"));}
    Ok(Some((record,file)))
}
impl PackageGuard {
    pub fn prepare_install(lease:Lease,new_version:&str)->Result<Self>{Self::prepare(lease,Some(version(new_version)?))}
    pub fn prepare_uninstall(lease:Lease)->Result<Self>{Self::prepare(lease,None)}
    fn prepare(lease:Lease,new_version:Option<[u32;3]>)->Result<Self>{
        if lease.mode!=Mode::ExclusiveInstaller{return Err(reject("package effects require exclusive lease"));}lease.validate()?;
        let record=load_record(&lease)?;let descriptor=security_descriptor(&lease.sid)?;
        let target=relative(&lease.pins[lease.pins.len()-2].file,OsStr::new(TARGET),true,new_version.is_some(),FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,descriptor.0)?;
        security(&target,&lease.sid)?;let target_id=identity(&target,true)?;
        let path=lease.launcher.parent().ok_or_else(||reject("missing native install parent"))?.join(TARGET);
        let mut guard=Self{lease,target,target_id,path,version:new_version.unwrap_or([0;3]),files:Vec::new(),record:None,uninstall:new_version.is_none(),started:false,finished:false};
        if let Some((prior,record_file))=record {
            guard.record=Some(record_file);
            if prior.target!=target_id{return Err(reject("registered install target identity changed"));}
            if new_version.is_some_and(|next|next<prior.version){return Err(reject("package downgrade is refused"));}
            if new_version.is_none(){guard.version=prior.version;if !prior.installed{return Err(reject("package is already uninstalled"));}}
            if prior.installed {
                if prior.files.len()!=2{return Err(reject("incomplete prior package inventory"));}
                for(name,entry)in FILES.iter().zip(prior.files.iter()){
                    if entry.name!=*name{return Err(reject("unknown prior package layout"));}
                    let file=relative(&guard.target,OsStr::new(name),false,false,FILE_GENERIC_READ|if guard.uninstall{DELETE}else{0},if guard.uninstall{FILE_SHARE_READ}else{FILE_SHARE_READ|FILE_SHARE_WRITE},ptr::null_mut())?;
                    security(&file,&guard.lease.sid)?;let id=identity(&file,false)?;let(bytes,sha)=read_hash(&file)?;
                    if id!=entry.identity||bytes!=entry.bytes||sha!=entry.sha256{return Err(reject("prior package file identity or bytes changed"));}
                    guard.files.push((name.to_string(),file,id));
                }
            }else if !prior.files.is_empty(){return Err(reject("invalid retired enrollment"));}
        }else{
            if guard.uninstall{return Err(reject("unregistered predecessor cannot be uninstalled"));}
            // Existing unrecognized contents are never treated as owned. The
            // native route is pinned; no recursive deletion follows this check.
            if std::fs::read_dir(&guard.path).map_err(|_|reject("inspect new install target"))?.next().is_some(){return Err(reject("unregistered nonempty install target"));}
        }
        guard.validate()?;Ok(guard)
    }
    /// Called only when the install section starts. Wizard cancellation before
    /// this boundary creates no executable placeholders or enrollment record.
    pub fn begin_install(&mut self)->Result<()> {
        if self.uninstall||self.started{return Err(reject("invalid install start boundary"));}
        self.validate()?;
        if self.files.is_empty(){
            let descriptor=security_descriptor(&self.lease.sid)?;
            for name in FILES {
                // FILE_CREATE rather than OPEN_IF: an occupant cannot become a
                // new owned file merely because its bytes happen to be empty.
                let file=create_new(&self.target,OsStr::new(name),FILE_GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE,descriptor.0)?;
                let id=identity(&file,false)?;self.files.push((name.into(),file,id));
            }
        }
        self.started=true;self.validate()
    }
    pub fn path(&self)->&Path{&self.path}
    pub fn validate(&self)->Result<()> {
        if self.finished{return Err(reject("package effect boundary already sealed"));}self.validate_held()
    }
    pub fn validate_held(&self)->Result<()> {
        self.lease.validate()?;
        if identity(&self.target,true)?!=self.target_id{return Err(reject("package directory changed"));}
        security(&self.target,&self.lease.sid)?;
        for(name,file,id)in &self.files{
            if identity(file,false)?!=*id{return Err(reject("held package artifact changed"));}security(file,&self.lease.sid)?;
            let reopened=relative(&self.target,OsStr::new(name),false,false,FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,ptr::null_mut())?;
            if identity(&reopened,false)?!=*id{return Err(reject("package artifact binding changed"));}
        }Ok(())
    }
    pub fn finish_install(&mut self)->Result<()> {
        if self.uninstall||!self.started||self.files.len()!=2{return Err(reject("install effects have not started"));}self.validate()?;
        let mut entries=Vec::new();for(name,file,id)in &self.files{let(bytes,sha256)=read_hash(file)?;if bytes==0{return Err(reject("package artifact is empty"));}entries.push(Entry{name:name.clone(),identity:*id,bytes,sha256});}
        self.write_record(true,entries)?;self.finished=true;Ok(())
    }
    pub fn remove_owned_files(&mut self)->Result<()> {
        if !self.uninstall{return Err(reject("install guard cannot delete package"));}self.validate()?;
        for(_,file,_)in &self.files{
            let mut disposition=FILE_DISPOSITION_INFO{DeleteFile:1};
            if unsafe{SetFileInformationByHandle(file.as_raw_handle(),FileDispositionInfo,(&mut disposition as *mut FILE_DISPOSITION_INFO).cast(),size_of::<FILE_DISPOSITION_INFO>()as u32)}==0{return Err(last_error("mark exact package artifact for removal"));}
        }
        self.files.clear();self.write_record(false,Vec::new())?;self.finished=true;Ok(())
    }
    pub fn lease(&self)->&Lease{&self.lease}
    fn write_record(&mut self,installed:bool,files:Vec<Entry>)->Result<()> {
        let value=Enrollment{schema_version:1,product:"local.imageranker.desktop".into(),target:self.target_id,version:self.version,installed,files};
        let bytes=serde_json::to_vec(&value).map_err(|_|reject("serialize package enrollment"))?;if bytes.len()>65536{return Err(reject("package enrollment exceeds bound"));}
        if self.record.is_none(){
            let parent=&self.lease.pins.last().unwrap().file;let descriptor=security_descriptor(&self.lease.sid)?;
            self.record=Some(create_new(parent,OsStr::new(RECORD),FILE_GENERIC_READ|FILE_GENERIC_WRITE,FILE_SHARE_READ,descriptor.0)?);
        }
        let file=self.record.as_mut().unwrap();identity(file,false)?;security(file,&self.lease.sid)?;
        // Rewrite the qualified held object only. Never drop authority and
        // replace a path occupant. An interrupted write remains explicitly
        // unqualified; it is not represented as an atomic package rollback.
        file.seek(SeekFrom::Start(0)).and_then(|_|file.write_all(&bytes)).and_then(|_|file.set_len(bytes.len()as u64)).and_then(|_|file.sync_all()).map_err(|_|reject("persist held package enrollment"))
    }
}
fn create_new(parent:&File,name:&OsStr,access:u32,share:u32,descriptor:*mut c_void)->Result<File>{
    let mut name:Vec<u16>=name.encode_wide().collect();if name.is_empty()||name.len()>255||name.iter().any(|c|matches!(*c,0|47|92|58)){return Err(reject("invalid package leaf"));}
    let mut unicode=UnicodeString{length:(name.len()*2)as u16,maximum_length:(name.len()*2)as u16,buffer:name.as_mut_ptr()};let mut attributes=ObjectAttributes{length:size_of::<ObjectAttributes>()as u32,root:parent.as_raw_handle(),name:&mut unicode,attributes:0x40,security_descriptor:descriptor,security_qos:ptr::null_mut()};let mut status=IoStatusBlock{status:0,information:0};let mut handle=ptr::null_mut();
    let code=unsafe{NtCreateFile(&mut handle,access|SYNCHRONIZE,&mut attributes,&mut status,ptr::null(),FILE_ATTRIBUTE_NORMAL,share,2,0x00200000|0x20|0x40,ptr::null(),0)};
    if code<0{return Err(os_error("create exclusive package leaf",unsafe{RtlNtStatusToDosError(code)}));}Ok(unsafe{File::from_raw_handle(handle)})
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn downgrade_order_is_numeric_and_ambiguous_versions_rejected(){assert!(version("0.10.0").unwrap()>version("0.9.9").unwrap());for v in ["1.0","1.0.0-beta","1.0.0.0","1.0.-1","99999999999.0.0"]{assert!(version(v).is_err());}}
    #[test]fn enrollment_cannot_gain_unknown_layout_fields(){assert!(serde_json::from_str::<Enrollment>(r#"{"schemaVersion":1,"product":"local.imageranker.desktop","target":{"volume":1,"high":0,"low":1},"version":[1,0,0],"installed":false,"files":[],"grant":"anything"}"#).is_err());}
    fn fixture()->PathBuf{
        let path=std::env::temp_dir().join(format!("image-ranker-package-test-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&path).unwrap();path
    }
    fn fixture_lease(path:&Path)->Lease{
        let sid=current_sid().unwrap();let descriptor=security_descriptor(&sid).unwrap();let path_wide=wide(path.as_os_str()).unwrap();
        let raw=unsafe{CreateFileW(path_wide.as_ptr(),FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,ptr::null(),OPEN_EXISTING,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT,ptr::null_mut())};assert_ne!(raw,INVALID_HANDLE_VALUE);
        let base=unsafe{File::from_raw_handle(raw)};let launcher=relative(&base,OsStr::new(LAUNCHER),true,true,FILE_READ_ATTRIBUTES|READ_CONTROL,FILE_SHARE_READ|FILE_SHARE_WRITE,descriptor.0).unwrap();
        let gate=relative(&launcher,OsStr::new(GATE),false,true,FILE_GENERIC_READ,0,descriptor.0).unwrap();
        let lease=Lease{mode:Mode::ExclusiveInstaller,sid,pins:vec![Pin{id:identity(&base,true).unwrap(),file:base,name:path.as_os_str().into(),owned:true},Pin{id:identity(&launcher,true).unwrap(),file:launcher,name:LAUNCHER.into(),owned:true}],gate_id:identity(&gate,false).unwrap(),gate,launcher:path.join(LAUNCHER)};lease.validate().unwrap();lease
    }
    fn cleanup(path:&Path){
        // Exact disposable fixture leaves only, never recursive cleanup.
        for leaf in ["Image Ranker/image-ranker.exe","Image Ranker/uninstall.exe","Image Ranker/preserve.txt","ImageRankerLauncher/install-layout-v1.json","ImageRankerLauncher/install-lifetime-v1.lock"]{let file=path.join(leaf);if file.exists(){std::fs::remove_file(file).unwrap();}}
        for directory in ["Image Ranker","ImageRankerLauncher"]{let value=path.join(directory);if value.exists(){std::fs::remove_dir(value).unwrap();}}std::fs::remove_dir(path).unwrap();
    }
    fn installed(path:&Path,version:&str){let mut guard=PackageGuard::prepare_install(fixture_lease(path),version).unwrap();guard.begin_install().unwrap();std::fs::write(guard.path().join(FILES[0]),b"fixture executable bytes").unwrap();std::fs::write(guard.path().join(FILES[1]),b"fixture uninstaller bytes").unwrap();guard.finish_install().unwrap();}
    #[test]fn wizard_cancellation_creates_no_executable_or_enrollment(){let path=fixture();let guard=PackageGuard::prepare_install(fixture_lease(&path),"1.0.0").unwrap();assert_eq!(std::fs::read_dir(guard.path()).unwrap().count(),0);assert!(!path.join(LAUNCHER).join(RECORD).exists());drop(guard);cleanup(&path);}
    #[test]fn exact_enrollment_allows_update_refuses_downgrade_and_preserves_unknown_files_on_uninstall(){
        let path=fixture();installed(&path,"1.0.0");std::fs::write(path.join(TARGET).join("preserve.txt"),b"unowned bytes").unwrap();
        installed(&path,"1.1.0");assert!(PackageGuard::prepare_install(fixture_lease(&path),"1.0.0").is_err());
        let mut remove=PackageGuard::prepare_uninstall(fixture_lease(&path)).unwrap();remove.remove_owned_files().unwrap();assert!(!path.join(TARGET).join(FILES[0]).exists());assert!(!path.join(TARGET).join(FILES[1]).exists());assert_eq!(std::fs::read(path.join(TARGET).join("preserve.txt")).unwrap(),b"unowned bytes");drop(remove);cleanup(&path);
    }
    #[test]fn changed_predecessor_bytes_and_unenrolled_occupants_are_never_overwritten(){
        let path=fixture();installed(&path,"1.0.0");let executable=path.join(TARGET).join(FILES[0]);std::fs::write(&executable,b"changed outside installation").unwrap();assert!(PackageGuard::prepare_install(fixture_lease(&path),"1.1.0").is_err());assert_eq!(std::fs::read(&executable).unwrap(),b"changed outside installation");cleanup(&path);
        let path=fixture();std::fs::create_dir(path.join(TARGET)).unwrap();std::fs::write(path.join(TARGET).join("preserve.txt"),b"unknown target").unwrap();assert!(PackageGuard::prepare_install(fixture_lease(&path),"1.0.0").is_err());assert_eq!(std::fs::read(path.join(TARGET).join("preserve.txt")).unwrap(),b"unknown target");cleanup(&path);
    }
}
