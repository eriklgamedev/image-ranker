//! Unicode NSIS in-process plugin. Source ABI reference: pinned v3.11
//! upstream/pluginapi.h. No shell, old-uninstaller, or process-close API exists.
#![cfg(windows)]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(non_snake_case)]
#[cfg(not(target_arch="x86"))]
compile_error!("The pinned NSIS Unicode host requires an i686 plugin, not the x64 application DLL");
use image_ranker_install_lifetime::{acquire_exclusive,PackageGuard};
use std::{ffi::c_void,mem::size_of,panic::{catch_unwind,AssertUnwindSafe},ptr,sync::{Mutex,OnceLock}};
use windows_sys::Win32::{Foundation::*,System::{LibraryLoader::*,Memory::*}};

#[repr(C)] pub struct Stack {next:*mut Stack,text:[u16;1]}
enum State { New, Held(PackageGuard), EffectsComplete(PackageGuard), Sealed }
static STATE:OnceLock<Mutex<State>>=OnceLock::new();
fn state()->&'static Mutex<State>{STATE.get_or_init(||Mutex::new(State::New))}

fn pin_module()->Result<(),String>{
    // PIN makes unload impossible until process exit. The native owner is a
    // static, not a transient NSIS stack object or a fallible helper process.
    let mut module=ptr::null_mut();
    if unsafe{GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS|GET_MODULE_HANDLE_EX_FLAG_PIN,AcquireInstall as *const () as *const u16,&mut module)}==0{return Err("INSTALLER_INTERLOCK_UNAVAILABLE: cannot pin plugin lifetime".into());}Ok(())
}
unsafe fn pop(stack:*mut *mut Stack,capacity:i32)->Result<String,String>{
    if stack.is_null()||capacity<2||capacity>32768{return Err("INSTALLER_PLUGIN_ABI: invalid Unicode stack".into());}
    let item=unsafe{*stack};if item.is_null(){return Err("INSTALLER_PLUGIN_ABI: missing argument".into());}
    let data=unsafe{ptr::addr_of!((*item).text).cast::<u16>()};let mut length=0;
    while length<capacity as usize && unsafe{*data.add(length)}!=0{length+=1;}
    if length==capacity as usize{return Err("INSTALLER_PLUGIN_ABI: unterminated argument".into());}
    let value=String::from_utf16(unsafe{std::slice::from_raw_parts(data,length)}).map_err(|_|"INSTALLER_PLUGIN_ABI: invalid Unicode".to_string());
    unsafe{*stack=(*item).next;GlobalFree(item.cast());}value
}
unsafe fn push(stack:*mut *mut Stack,capacity:i32,text:&str){
    if stack.is_null()||capacity<2||capacity>32768{return;}
    let text:Vec<u16>=text.encode_utf16().collect();if text.len()>=capacity as usize{return;}
    let bytes=size_of::<*mut Stack>()+(capacity as usize)*2;
    let node=unsafe{GlobalAlloc(GPTR,bytes)}.cast::<Stack>();if node.is_null(){return;}
    unsafe{(*node).next=*stack;ptr::copy_nonoverlapping(text.as_ptr(),ptr::addr_of_mut!((*node).text).cast::<u16>(),text.len());*stack=node;}
}
unsafe fn invoke(stack:*mut *mut Stack,capacity:i32,action:impl FnOnce()->Result<Option<String>,String>){
    let result=catch_unwind(AssertUnwindSafe(action)).unwrap_or_else(|_|Err("INSTALLER_INTERLOCK_UNAVAILABLE: plugin panic; effects stopped".into()));
    match result{Ok(value)=>{if let Some(value)=value{unsafe{push(stack,capacity,&value)}}unsafe{push(stack,capacity,"OK")}},Err(error)=>unsafe{push(stack,capacity,&error)}}
}
fn with_state(action:impl FnOnce(&mut State)->Result<Option<String>,String>)->Result<Option<String>,String>{
    let mut guard=state().lock().map_err(|_|"INSTALLER_INTERLOCK_UNAVAILABLE: poisoned owner".to_string())?;action(&mut guard)
}

#[no_mangle]pub unsafe extern "C" fn AcquireInstall(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){
    unsafe{invoke(stack,capacity,||{
        pin_module()?;let version=unsafe{pop(stack,capacity)}?;
        with_state(|state|{if !matches!(state,State::New){return Err("INSTALLER_INTERLOCK_UNAVAILABLE: duplicate acquisition".into());}
            let lease=acquire_exclusive().map_err(|e|e.to_string())?;
            let package=PackageGuard::prepare_install(lease,&version).map_err(|e|e.to_string())?;
            let path=package.path().to_str().ok_or("INSTALLER_INTERLOCK_UNAVAILABLE: install path is not Unicode")?.to_string();
            *state=State::Held(package);Ok(Some(path))})
    })}
}
#[no_mangle]pub unsafe extern "C" fn AcquireUninstall(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){
    unsafe{invoke(stack,capacity,||{pin_module()?;with_state(|state|{
        if !matches!(state,State::New){return Err("INSTALLER_INTERLOCK_UNAVAILABLE: duplicate acquisition".into());}
        let lease=acquire_exclusive().map_err(|e|e.to_string())?;let package=PackageGuard::prepare_uninstall(lease).map_err(|e|e.to_string())?;
        let path=package.path().to_str().ok_or("INSTALLER_INTERLOCK_UNAVAILABLE: install path is not Unicode")?.to_string();*state=State::Held(package);Ok(Some(path))
    })})}
}
#[no_mangle]pub unsafe extern "C" fn ValidateHeld(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){
    unsafe{invoke(stack,capacity,||with_state(|state|{match state{
        State::Held(package)=>package.validate().map_err(|e|e.to_string())?,
        State::EffectsComplete(package)=>package.validate_held().map_err(|e|e.to_string())?,
        _=>return Err("INSTALLER_INTERLOCK_UNAVAILABLE: no active effect owner".into())};Ok(None)}))}
}
fn finish(uninstall:bool)->Result<Option<String>,String>{with_state(|state|{
    if let State::Held(package)=state{(if uninstall{package.remove_owned_files()}else{package.finish_install()}).map_err(|e|e.to_string())?;}else{return Err("INSTALLER_INTERLOCK_UNAVAILABLE: no mutable package owner".into());}
    let prior=std::mem::replace(state,State::Sealed);if let State::Held(package)=prior{*state=State::EffectsComplete(package);}Ok(None)
})}
#[no_mangle]pub unsafe extern "C" fn BeginInstall(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){
    unsafe{invoke(stack,capacity,||with_state(|state|{if let State::Held(package)=state{package.begin_install().map_err(|e|e.to_string())?;Ok(None)}else{Err("INSTALLER_INTERLOCK_UNAVAILABLE: no install owner".into())}}))}
}
#[no_mangle]pub unsafe extern "C" fn CompleteInstall(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){unsafe{invoke(stack,capacity,||finish(false))}}
#[no_mangle]pub unsafe extern "C" fn RemoveOwnedFiles(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){unsafe{invoke(stack,capacity,||finish(true))}}
#[no_mangle]pub unsafe extern "C" fn ReleaseAfterEffects(_window:HWND,capacity:i32,_variables:*mut u16,stack:*mut *mut Stack,_extra:*mut c_void){
    unsafe{invoke(stack,capacity,||with_state(|state|{
        if let State::EffectsComplete(package)=state{package.validate_held().map_err(|e|e.to_string())?;}else if matches!(state,State::Sealed){return Ok(None);}else{return Err("INSTALLER_INTERLOCK_UNAVAILABLE: effect completion unconfirmed".into());}
        *state=State::Sealed;Ok(None)
    }))}
}
