//! Cooperative process-lifetime installer exclusion. No profile or renderer path
//! supplies authority. Integration and actual Windows qualification are separate.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{acquire_exclusive, acquire_shared, Lease, PackageGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode { SharedApplication, ExclusiveInstaller }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error { pub code: &'static str, pub stage: &'static str, pub os_code: Option<u32> }
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.stage)?;
        if let Some(code) = self.os_code { write!(f, " (Windows {code})")?; }
        Ok(())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

// Keep policy decisions testable without inventing OS ownership.
fn harmful_grant(mask: u32) -> bool {
    mask & (0x4000_0000 | 0x1000_0000 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 2 | 4 | 16 | 64 | 256) != 0
}
fn trusted_sid(sid: &str, current: &str) -> bool {
    sid == current || sid == "S-1-5-18" || sid == "S-1-5-32-544"
}
fn trusted_route_owner(sid: &str, current: &str) -> bool {
    trusted_sid(sid,current) || sid == "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
}
fn harmful_route_grant(mask:u32)->bool {
    // Creating another child does not replace the existing qualified child,
    // whose actual handle is pinned without delete sharing. Do not widen this
    // exception to generic write, attributes, delete-child, delete or ACL rights.
    harmful_grant(mask & !(2|4))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_modify_delete_acl_and_generic_grants_are_rejected_by_policy() {
        for mask in [2,4,16,64,256,0x10000,0x40000,0x80000,0x40000000,0x10000000] { assert!(harmful_grant(mask)); }
        for mask in [1,8,32,128,0x20000,0x100000,0x80000000] { assert!(!harmful_grant(mask)); }
    }
    #[test]
    fn trust_is_exact_current_sid_system_or_builtin_admins() {
        assert!(trusted_sid("S-1-5-21-7", "S-1-5-21-7"));
        assert!(trusted_sid("S-1-5-18", "S-1-5-21-7"));
        assert!(!trusted_sid("S-1-5-21-70", "S-1-5-21-7"));
        assert!(!trusted_sid("S-1-1-0", "S-1-5-21-7"));
    }
    #[test]
    fn os_ancestor_policy_allows_sibling_creation_but_not_bound_route_modification() {
        assert!(!harmful_route_grant(2|4));
        for mask in [16,64,256,0x10000,0x40000,0x80000,0x40000000,0x10000000] {assert!(harmful_route_grant(mask));}
        let installer="S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
        assert!(trusted_route_owner(installer,"S-1-5-21-7"));assert!(!trusted_sid(installer,"S-1-5-21-7"));
        assert!(!trusted_route_owner("S-1-5-21-8","S-1-5-21-7"));
    }
}
