use super::*;
use std::time::Instant;

fn counters() -> (usize, usize, usize, usize) {
    (
        CACHE_ENUMERATIONS.with(|n| n.get()),
        CACHE_ENTRY_INSPECTIONS.with(|n| n.get()),
        CACHE_METADATA_READS.with(|n| n.get()),
        CACHE_RETIREMENT_ATTEMPTS.with(|n| n.get()),
    )
}
fn reset_counts() {
    CACHE_ENUMERATIONS.with(|n| n.set(0));
    CACHE_ENTRY_INSPECTIONS.with(|n| n.set(0));
    CACHE_METADATA_READS.with(|n| n.set(0));
    CACHE_RETIREMENT_ATTEMPTS.with(|n| n.set(0));
}

// Fixture construction is timed separately. Receipts record actual created file
// identities; production receipt/identity validation still owns all retirement.
fn owned_pair(
    cache: &Path,
    index: usize,
    png: &[u8],
    digest: &str,
    parent: &str,
) -> (PathBuf, u64) {
    let path = cache.join(format!("{index:064x}.png"));
    let mut file = File::create(&path).unwrap();
    file.write_all(png).unwrap();
    let metadata_path = cache_metadata_path(&path);
    let mut sidecar = File::create(&metadata_path).unwrap();
    let metadata = PreviewMetadata {
        schema_version: 2,
        pipeline_version: PIPELINE_VERSION.into(),
        png_sha256: digest.into(),
        width: 1,
        height: 1,
        png_identity: crate::operations::handle_identity(&file).unwrap(),
        metadata_identity: crate::operations::handle_identity(&sidecar).unwrap(),
        parent_identity: parent.into(),
    };
    let bytes = serde_json::to_vec(&metadata).unwrap();
    sidecar.write_all(&bytes).unwrap();
    (path, png.len() as u64 + bytes.len() as u64)
}

#[test]
fn real_cache_inventory_scales_at_100_1000_10000_and_foreign_files_and_shared_pins_survive() {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([40, 80, 120])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let digest = format!("{:x}", Sha256::digest(&png));
    for n in [100, 1_000, 10_000] {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("cache");
        let mut catalog = Catalog::new(cache_root.clone());
        let directory = catalog.cache_dir.clone();
        fs::create_dir_all(&directory).unwrap();
        let parent = crate::operations::directory_identity(&directory).unwrap();
        let started = Instant::now();
        let mut paths = Vec::with_capacity(n);
        let mut expected_bytes = 0;
        for index in 0..n {
            let (path, bytes) = owned_pair(&directory, index, &png, &digest, &parent);
            paths.push((path, bytes));
            expected_bytes += bytes;
        }
        let foreign = directory.join("unowned-photo.png");
        fs::write(&foreign, &png).unwrap();
        expected_bytes += png.len() as u64;
        let foreign_identity =
            crate::operations::handle_identity(&File::open(&foreign).unwrap()).unwrap();
        let setup_us = started.elapsed().as_micros();
        reset_counts();
        let started = Instant::now();
        let usage = catalog.cache_usage().unwrap();
        let inventory_us = started.elapsed().as_micros();
        assert_eq!(usage.files, n + 1);
        assert_eq!(usage.size_bytes, expected_bytes);
        assert_eq!(usage.temporary_bytes, 0);
        assert_eq!(usage.pixel_reserved_bytes, 0);
        assert_eq!(counters(), (1, 2 * n + 1, 4 * n + 2, 0));
        let retained = catalog
            .cache_state
            .lock()
            .unwrap()
            .inventory
            .as_ref()
            .unwrap()
            .retained_counts();
        assert_eq!(retained.0, n + 1);
        assert_eq!(retained.1, n + 1);
        assert!(retained.2 <= 2 * (n + 1));
        reset_counts();
        let started = Instant::now();
        {
            let mut state = catalog.cache_state.lock().unwrap();
            for (path, _) in &paths {
                state.inventory.as_mut().unwrap().touch(path);
            }
        }
        catalog.prune_cache(None).unwrap();
        let warm_us = started.elapsed().as_micros();
        assert_eq!(
            counters(),
            (0, 0, 0, 0),
            "warm per-entry index operations must not enumerate"
        );
        let mut leases = Vec::new();
        for _ in 0..2 {
            let held = open_valid_cache(&paths[0].0).unwrap();
            let id = Uuid::new_v4().to_string();
            let mut state = catalog.cache_state.lock().unwrap();
            state.pixel_bytes += 8;
            state.leases.insert(
                id.clone(),
                PreviewLease {
                    path: held.path,
                    bytes: held.bytes,
                    pixel_bytes: 8,
                    _file: held.file,
                    _parents: held.parents,
                },
            );
            leases.push(id);
        }
        catalog.cache_budget = paths[0].1 + png.len() as u64;
        let started = Instant::now();
        catalog.prune_cache(None).unwrap();
        let eviction_us = started.elapsed().as_micros();
        let eviction_counts = counters();
        assert_eq!(eviction_counts.0, 0);
        assert_eq!(eviction_counts.1, 0);
        assert_eq!(eviction_counts.2, 0);
        assert!((n - 1..=n).contains(&eviction_counts.3));
        let held_usage = catalog.cache_usage().unwrap();
        assert_eq!(held_usage.files, 2);
        assert_eq!(held_usage.pinned_files, 1);
        assert_eq!(held_usage.pixel_reserved_bytes, 16);
        catalog.cache_budget = png.len() as u64;
        catalog.release_preview(&leases[0]).unwrap();
        drain_cache_for_test(&catalog);
        assert!(paths[0].0.exists());
        catalog.release_preview(&leases[1]).unwrap();
        drain_cache_for_test(&catalog);
        assert!(!paths[0].0.exists());
        let usage = catalog.cache_usage().unwrap();
        assert_eq!(usage.files, 1);
        assert_eq!(usage.size_bytes, png.len() as u64);
        assert_eq!(usage.pixel_reserved_bytes, 0);
        assert_eq!(usage.pinned_files, 0);
        assert_eq!(fs::read(&foreign).unwrap(), png);
        assert_eq!(
            crate::operations::handle_identity(&File::open(&foreign).unwrap()).unwrap(),
            foreign_identity
        );
        drop(catalog);
        let restarted = Catalog::new(cache_root);
        assert_eq!(
            restarted.cache_usage().unwrap().size_bytes,
            png.len() as u64
        );
        // Logical instrumentation, not an OS syscall trace or a heap/RSS claim.
        println!(
            "CACHE_SCALE {}",
            serde_json::json!({"ownedPairs":n,"setupUs":setup_us,
            "coldInventoryUs":inventory_us,"coldEnumerations":1,"coldEntries":2*n+1,
            "coldMetadataCalls":4*n+2,"warmIndexUs":warm_us,"warmEnumerations":0,
            "evictionUs":eviction_us,"retirementAttempts":eviction_counts.3,
            "retainedIndexEntries":retained.0,"retainedLruEntries":retained.1,
            "indexMapCapacity":retained.2,"retainedPathBytes":retained.3,
            "initialDiskBytes":expected_bytes,"finalForeignBytes":png.len()})
        );
    }
}
