use super::*;

#[test]
fn successful_shutdown_seals_all_renderer_admission_and_exact_request_identity() {
    let context = PreviewContext::new();
    let original = context.current().unwrap();
    assert_eq!(
        context
            .seal_with(&original, "finished-native-request", || Ok(17))
            .unwrap(),
        17
    );
    assert!(context
        .is_sealed_for(&original, "finished-native-request")
        .unwrap());
    assert!(!context
        .is_sealed_for(&original, "different-request")
        .unwrap());
    assert!(!context
        .is_sealed_for("different-context", "finished-native-request")
        .unwrap());
    assert!(context
        .current()
        .unwrap_err()
        .starts_with("NATIVE_SHUTDOWN_SEALED"));
    assert!(context
        .rotate()
        .unwrap_err()
        .starts_with("NATIVE_SHUTDOWN_SEALED"));
    assert!(context
        .validate(&original)
        .unwrap_err()
        .starts_with("NATIVE_SHUTDOWN_SEALED"));
    let called = AtomicBool::new(false);
    assert!(context
        .with_valid(&original, || called.store(true, Ordering::SeqCst))
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert!(context
        .validate(&PreviewContext::shutdown_token(
            &original,
            "finished-native-request"
        ))
        .is_err());
}

#[test]
fn failed_or_panicked_close_restores_exact_context_without_poison_or_approval() {
    let context = PreviewContext::new();
    let original = context.current().unwrap();
    assert!(context
        .seal_with::<()>(&original, "request", || Err("WINDOW_CLOSE_FAILED".into()))
        .is_err());
    assert_eq!(context.current().unwrap(), original);
    assert!(!context.is_sealed_for(&original, "request").unwrap());
    assert!(context
        .seal_with::<()>(&original, "request", || panic!(
            "actual synchronous close callback unwind"
        ))
        .is_err());
    assert!(!context.0.is_poisoned());
    assert_eq!(context.current().unwrap(), original);
    context.validate(&original).unwrap();
    assert!(context
        .seal_with::<()>("stale", "request", || panic!(
            "stale request must never enter callback"
        ))
        .is_err());
    assert_eq!(context.current().unwrap(), original);
}
