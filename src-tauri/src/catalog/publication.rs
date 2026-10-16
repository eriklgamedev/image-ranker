//! Parent-side cache publication runs in the admitted image worker, never under
//! Backend or cache coordination. Exact created handles survive lease transfer.
use super::*;

struct Writer {
    work: CacheMaintenance,
    path: PathBuf,
    bytes: u64,
    committed: bool,
}
impl Drop for Writer {
    fn drop(&mut self) {
        // The publication closure and its held artifacts have ended before this
        // guard. Unremoved partials keep the conservative disk reservation.
        let retained = !self.committed
            && (!matches!(cache_presence(&self.path), Ok(false))
                || !matches!(cache_presence(&cache_metadata_path(&self.path)), Ok(false)));
        if let Ok(mut cache) = self.work.cache.lock() {
            cache.maintenance.writers.remove(&self.path);
            cache.maintenance.waiting_for_paths = false;
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            if retained {
                cache
                    .maintenance
                    .failed_writers
                    .insert(self.path.clone(), self.bytes);
                cache.pending_delete.insert(self.path.clone());
            }
            if !self.committed {
                cache.maintenance.refresh = true;
            }
            cache.maintenance.prune = true;
        }
        self.work.wake();
    }
}
impl CacheMaintenance {
    pub(super) fn publish(
        &self,
        source: &Path,
        edge: u32,
        mut work: SourceDecode,
    ) -> Result<ValidatedCache, String> {
        #[cfg(test)]
        {
            let hook = self
                .cache
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
                .before_publication
                .clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let canonical = preview_key(
            &self.directory,
            source,
            &work.decoded.info.source_sha256,
            edge,
            work.generation,
        );
        let existing = {
            let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.generation != work.generation {
                return Err("CACHE_CLEARED: discarded stale preview publication".into());
            }
            cache.aliases.get(&canonical).unwrap_or(&canonical).clone()
        };
        if let Ok(pin) = maintenance::PathPin::acquire(self, &existing) {
            if let Ok(mut held) = open_valid_cache(&existing) {
                if held.metadata.png_sha256 == work.decoded.info.output_sha256 {
                    held.reservation = Some(pin);
                    return Ok(held);
                }
            }
        }
        let destination = if existing.exists()
            || cache_metadata_path(&existing).exists()
            || self
                .cache
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
                .maintenance
                .retiring
                .contains(&existing)
        {
            let key = format!("{:x}", Sha256::digest(Uuid::new_v4().as_bytes()));
            self.directory
                .join(format!("{:016x}{key}.png", work.generation))
        } else {
            existing
        };
        let pin = maintenance::PathPin::acquire(self, &destination)?;
        self.refresh_wait(&work.cancel)?;
        let needed = work.decoded.info.output_bytes.saturating_add(4096);
        let budget = self
            .cache
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
            .budget;
        {
            self.cache
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
                .maintenance
                .prune = true;
        }
        let maintenance = self.drain(
            &work.cancel,
            Some(budget.saturating_sub(needed)),
            Some(&destination),
        );
        if let Err(error) = maintenance {
            let snapshot = self.snapshot()?;
            // Unowned/externally held occupants remain charged. A recorded
            // retirement warning need not prevent an unrelated preview if the
            // authoritative inventory still proves sufficient reserved space.
            if work.cancel.load(Ordering::SeqCst)
                || error.starts_with("CACHE_CANCELLED")
                || error.starts_with("CACHE_MAINTENANCE_")
                || !snapshot.inventory_known
                || snapshot.maintenance_running
                || snapshot.maintenance_pending
            {
                return Err(error);
            }
        }
        #[cfg(test)]
        let fault;
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.generation != work.generation || work.cancel.load(Ordering::SeqCst) {
                return Err("CACHE_CLEARED: publication retired before writing".into());
            }
            let index = cache.inventory.as_ref().ok_or("CACHE_STATE_UNAVAILABLE")?;
            if index.len() + cache.maintenance.writers.len() >= MAX_CACHE_ENTRIES
                || cache.aliases.len() >= 4096
            {
                return Err("CACHE_CAPACITY: retained cache entries require review".into());
            }
            if index
                .bytes
                .saturating_add(cache.maintenance.reserved_bytes())
                .saturating_add(needed)
                > cache.budget
            {
                return Err("CACHE_CAPACITY: retained or displayed artifacts leave insufficient publication space".into());
            }
            if cache.maintenance.retiring.contains(&destination)
                || cache.maintenance.writers.contains_key(&destination)
            {
                return Err("CACHE_PATH_BUSY".into());
            }
            cache
                .maintenance
                .writers
                .insert(destination.clone(), needed);
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            #[cfg(test)]
            {
                fault = cache.publication_fault.clone();
            }
        }
        let mut writer = Writer {
            work: self.clone(),
            path: destination.clone(),
            bytes: needed,
            committed: false,
        };
        let parent_identity = crate::operations::directory_identity(&self.directory)?;
        let mut png = artifact::Artifact::create(&destination, &parent_identity)?;
        let png_identity = crate::operations::handle_identity(&png.file)?;
        work.decoded
            .output
            .seek(SeekFrom::Start(0))
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        let mut digest = Sha256::new();
        loop {
            if work.cancel.load(Ordering::SeqCst) {
                return Err("DECODE_CANCELLED".into());
            }
            let count = work
                .decoded
                .output
                .read(&mut buffer)
                .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > work.decoded.info.output_bytes {
                return Err("DECODE_PROTOCOL: publication length changed".into());
            }
            digest.update(&buffer[..count]);
            png.file
                .write_all(&buffer[..count])
                .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
            #[cfg(test)]
            if let Some(hook) = &fault {
                hook("png-write", &destination)?;
            }
        }
        if total != work.decoded.info.output_bytes
            || format!("{:x}", digest.finalize()) != work.decoded.info.output_sha256
        {
            return Err(
                "DECODE_PROTOCOL: parent publication does not match verified worker output".into(),
            );
        }
        png.file
            .sync_all()
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        #[cfg(test)]
        if let Some(hook) = &fault {
            hook("png-synced", &destination)?;
        }
        let metadata_path = cache_metadata_path(&destination);
        let mut sidecar = artifact::Artifact::create(&metadata_path, &parent_identity)?;
        let metadata = PreviewMetadata {
            schema_version: 2,
            pipeline_version: PIPELINE_VERSION.into(),
            png_sha256: work.decoded.info.output_sha256.clone(),
            width: work.decoded.info.output_width,
            height: work.decoded.info.output_height,
            png_identity,
            metadata_identity: crate::operations::handle_identity(&sidecar.file)?,
            parent_identity,
        };
        serde_json::to_writer(&mut sidecar.file, &metadata)
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        if work.cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        #[cfg(test)]
        if let Some(hook) = &fault {
            hook("metadata-written", &metadata_path)?;
        }
        sidecar.retain()?;
        #[cfg(test)]
        if let Some(hook) = &fault {
            hook("receipt-retained", &metadata_path)?;
        }
        png.retain()?;
        let published_bytes = total.saturating_add(
            sidecar
                .file
                .metadata()
                .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?
                .len(),
        );
        // Windows share checks prevent opening a deny-write consumer while the
        // creation handle still requests write access. Keep the pinned route,
        // remembered created identities and path reservation across handoff,
        // then revalidate digest/identity on the exact deny-write read handle.
        sidecar.disarm();
        let route = png.finish_with_route();
        #[cfg(test)]
        if let Some(hook) = &fault {
            hook("creator-closed", &destination)?;
        }
        let mut held = open_valid_cache(&destination)?;
        if held.metadata.png_identity != metadata.png_identity
            || held.metadata.metadata_identity != metadata.metadata_identity
            || held.metadata.parent_identity != metadata.parent_identity
            || held.metadata.png_sha256 != metadata.png_sha256
        {
            return Err(
                "CACHE_CORRUPT: created cache identity changed during read-handle handoff".into(),
            );
        }
        held.parents.extend(route);
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.generation != work.generation || work.cancel.load(Ordering::SeqCst) {
                return Err("CACHE_CLEARED: publication generation retired during writing".into());
            }
            cache
                .inventory
                .as_mut()
                .ok_or("CACHE_STATE_UNAVAILABLE")?
                .insert(destination.clone(), published_bytes, SystemTime::now());
            cache
                .inventory
                .as_mut()
                .unwrap()
                .mark_publication(&destination, work.generation);
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            if destination != canonical {
                cache.aliases.insert(canonical, destination.clone());
            }
        }
        writer.committed = true;
        held.reservation = Some(pin);
        Ok(held)
    }
}
