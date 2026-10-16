//! Startup opens independent protection off the coordinator, then publishes once.
use crate::backup::protection::ProtectionRegistry;
use std::sync::OnceLock;

pub(crate) struct RegistrySlot {
    value: OnceLock<Result<ProtectionRegistry, String>>,
    opening: String,
}
impl RegistrySlot {
    pub(crate) fn opening() -> Self {
        Self {
            value: OnceLock::new(),
            opening: "PROTECTION_LOADING: independent protection is opening".into(),
        }
    }
    pub(crate) fn install(&self, value: Result<ProtectionRegistry, String>) -> Result<(), String> {
        self.value
            .set(value)
            .map_err(|_| "PROTECTION_ALREADY_INITIALIZED".into())
    }
    pub(crate) fn as_ref(&self) -> Result<&ProtectionRegistry, &String> {
        self.value
            .get()
            .map(Result::as_ref)
            .unwrap_or(Err(&self.opening))
    }
    pub(crate) fn clone(&self) -> Result<ProtectionRegistry, String> {
        self.as_ref().cloned().map_err(Clone::clone)
    }
    pub(crate) fn is_ok(&self) -> bool {
        self.as_ref().is_ok()
    }
}
impl From<Result<ProtectionRegistry, String>> for RegistrySlot {
    fn from(value: Result<ProtectionRegistry, String>) -> Self {
        let slot = Self::opening();
        let _ = slot.install(value);
        slot
    }
}
