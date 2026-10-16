use super::*;

fn folder(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn empty_profile_restart_retains_marker_and_distinct_writer_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = folder(temp.path(), "profile");
    let first = OwnedProfile::create(&root).unwrap();
    let expected = first.selection();
    let marker = fs::read(root.join(MARKER)).unwrap();
    assert!(!first.location.is_default);
    assert!(first.location.cache.ends_with("cache"));
    drop(first);
    let second = OwnedProfile::open(&root, None).unwrap();
    assert!(second.matches_selection(&expected));
    assert_eq!(fs::read(root.join(MARKER)).unwrap(), marker);
    second.validate().unwrap();
}

#[test]
fn arbitrary_occupied_folder_is_never_initialized_or_modified() {
    let temp = tempfile::tempdir().unwrap();
    let root = folder(temp.path(), "occupied");
    fs::write(root.join("unrelated.txt"), b"user-owned").unwrap();
    assert!(OwnedProfile::create(&root)
        .err()
        .unwrap()
        .contains("PROFILE_NOT_EMPTY"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(fs::read(root.join("unrelated.txt")).unwrap(), b"user-owned");
    assert!(OwnedProfile::open(&root, None).is_err());
}

#[test]
fn explicit_open_never_enrolls_an_unmarked_existing_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = folder(temp.path(), "unmarked");
    fs::write(root.join("session.json"), b"evidence").unwrap();
    assert!(OwnedProfile::open(&root, None).is_err());
    assert!(!root.join(WRITER).exists());
    assert!(!root.join(MARKER).exists());
}

#[test]
fn same_byte_marker_and_writer_replacements_are_rejected_on_reopen() {
    for name in [MARKER, WRITER, "cache/.image-ranker-cache.lock"] {
        let temp = tempfile::tempdir().unwrap();
        let root = folder(temp.path(), "profile");
        drop(OwnedProfile::create(&root).unwrap());
        let target = root.join(name);
        let bytes = fs::read(&target).unwrap();
        fs::rename(&target, target.with_extension("old")).unwrap();
        fs::write(&target, &bytes).unwrap();
        assert!(
            OwnedProfile::open(&root, None).is_err(),
            "replacement accepted: {name}"
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }
}

#[test]
fn marker_file_and_profile_ancestors_are_pinned_for_entire_lease() {
    let temp = tempfile::tempdir().unwrap();
    let ancestor = folder(temp.path(), "ancestor");
    let root = folder(&ancestor, "profile");
    let owned = OwnedProfile::create(&root).unwrap();
    assert!(fs::rename(root.join(MARKER), root.join("marker-old")).is_err());
    assert!(fs::write(root.join(MARKER), b"replacement").is_err());
    assert!(fs::rename(&root, ancestor.join("replacement")).is_err());
    assert!(fs::rename(&ancestor, temp.path().join("other-parent")).is_err());
    owned.validate().unwrap();
}

#[test]
fn second_writer_is_refused_but_distinct_profile_is_independent() {
    let temp = tempfile::tempdir().unwrap();
    let root = folder(temp.path(), "one");
    let other = folder(temp.path(), "two");
    let first = OwnedProfile::create(&root).unwrap();
    assert!(OwnedProfile::open(&root, None).is_err());
    let second = OwnedProfile::create(&other).unwrap();
    assert_ne!(first.selection().profile_id, second.selection().profile_id);
    drop(first);
    OwnedProfile::open(&root, None).unwrap();
    second.validate().unwrap();
}

#[test]
fn cancel_candidate_releases_only_its_lease_and_preserves_current_profile() {
    let temp = tempfile::tempdir().unwrap();
    let current = OwnedProfile::create(&folder(temp.path(), "current")).unwrap();
    let candidate_root = folder(temp.path(), "candidate");
    let candidate = OwnedProfile::create(&candidate_root).unwrap();
    let current_marker = fs::read(current.location.data.join(MARKER)).unwrap();
    let candidate_marker = fs::read(candidate_root.join(MARKER)).unwrap();
    drop(candidate);
    current.validate().unwrap();
    let reopened = OwnedProfile::open(&candidate_root, None).unwrap();
    assert_eq!(
        fs::read(current.location.data.join(MARKER)).unwrap(),
        current_marker
    );
    assert_eq!(
        fs::read(candidate_root.join(MARKER)).unwrap(),
        candidate_marker
    );
    assert!(OwnedProfile::open(&current.location.data, None).is_err());
    drop(reopened);
}

#[test]
fn preparation_failure_and_unavailable_original_do_not_require_original_ack() {
    let temp = tempfile::tempdir().unwrap();
    let failed = temp.path().join("unavailable");
    fs::write(&failed, b"unavailable profile evidence").unwrap();
    let old = ProfileOwner::unavailable(
        ProfileLocation {
            data: failed.clone(),
            cache: failed.join("cache"),
            is_default: true,
            valid: true,
        },
        None,
    );
    assert!(old.validate().is_err());
    assert!(!old.summary().writable);
    let occupied = folder(temp.path(), "failed-preparation");
    fs::write(occupied.join("evidence"), b"preserve").unwrap();
    assert!(OwnedProfile::create(&occupied).is_err());
    assert_eq!(fs::read(&failed).unwrap(), b"unavailable profile evidence");
    let candidate = OwnedProfile::create(&folder(temp.path(), "healthy")).unwrap();
    let new = ProfileOwner::acquired(candidate);
    new.validate().unwrap();
    assert!(new.summary().writable);
    assert_ne!(new.generation, old.generation);
    assert_eq!(fs::read(&failed).unwrap(), b"unavailable profile evidence");
}

#[test]
fn copied_profile_marker_does_not_grant_replacement_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = folder(temp.path(), "profile");
    drop(OwnedProfile::create(&root).unwrap());
    let renamed = temp.path().join("old-profile");
    fs::rename(&root, &renamed).unwrap();
    fs::create_dir(&root).unwrap();
    for entry in fs::read_dir(&renamed).unwrap() {
        let entry = entry.unwrap();
        fs::rename(entry.path(), root.join(entry.file_name())).unwrap();
    }
    assert!(OwnedProfile::open(&root, None)
        .err()
        .unwrap()
        .contains("PROFILE_IDENTITY_CHANGED"));
}

#[test]
fn default_legacy_enrollment_preserves_existing_state_and_reopens_native_cache() {
    let temp = tempfile::tempdir().unwrap();
    let data = folder(temp.path(), "default");
    let cache = temp.path().join("default-cache");
    fs::write(data.join(WRITER), b"").unwrap();
    fs::write(data.join("legacy-session.json"), b"legacy evidence").unwrap();
    let first = OwnedProfile::default_profile(&data, &cache).unwrap();
    let expected = first.selection();
    assert!(first.location.is_default);
    drop(first);
    assert!(OwnedProfile::open(&data, None).is_err());
    let reopened = OwnedProfile::default_profile(&data, &cache).unwrap();
    assert!(reopened.matches_selection(&expected));
    assert_eq!(
        fs::read(data.join("legacy-session.json")).unwrap(),
        b"legacy evidence"
    );
}

#[test]
fn launcher_selection_reopens_latest_profile_independently_of_broken_default() {
    let temp = tempfile::tempdir().unwrap();
    let launcher = temp.path().join("launcher");
    let one = OwnedProfile::create(&folder(temp.path(), "one")).unwrap();
    let two = OwnedProfile::create(&folder(temp.path(), "two")).unwrap();
    assert!(load_selection(&launcher).unwrap().is_none());
    save_selection(&launcher, &one).unwrap();
    save_selection(&launcher, &two).unwrap();
    let selected = load_selection(&launcher).unwrap().unwrap();
    assert_eq!(selected, two.selection());
    let log = launcher.join("selected-profile.jsonl");
    let bytes = fs::read(&log).unwrap();
    save_selection(&launcher, &two).unwrap();
    assert_eq!(fs::read(&log).unwrap(), bytes);
    drop(two);
    let reopened = OwnedProfile::open(&selected.data_root, None).unwrap();
    assert!(reopened.matches_selection(&selected));
}

#[test]
fn launcher_replacement_and_torn_suffix_preserve_evidence_and_never_grant_writes() {
    for replacement in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let launcher = temp.path().join("launcher");
        let profile = OwnedProfile::create(&folder(temp.path(), "profile")).unwrap();
        save_selection(&launcher, &profile).unwrap();
        let path = launcher.join("selected-profile.jsonl");
        if replacement {
            let bytes = fs::read(&path).unwrap();
            fs::rename(&path, launcher.join("old.jsonl")).unwrap();
            fs::write(&path, bytes).unwrap();
        } else {
            OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"{partial")
                .unwrap();
        }
        let evidence = fs::read(&path).unwrap();
        assert!(load_selection(&launcher).is_err());
        assert!(save_selection(&launcher, &profile).is_err());
        assert_eq!(fs::read(&path).unwrap(), evidence);
        profile.validate().unwrap();
    }
}
