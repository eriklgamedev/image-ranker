//! Qualified read-only discovery of an interrupted native selection request.
use super::*;

pub(crate) struct PendingSelection {
    pub token: String,
    pub disposition: String,
    pub selected: Selection,
    pub authorization: Option<Authorization>,
    pub origin: Option<Selection>,
    _pins: Vec<File>,
    _log: File,
}

pub(crate) fn discover_pending(directory: &Path) -> Result<Option<PendingSelection>, String> {
    if !exists(&directory.join(FILE))? {
        return Ok(None);
    }
    let root = qualified_root(directory)?;
    let parent = directory_identity(&root)?;
    let mut pins = pin_chain(&root)?;
    let (mut log, log_pins) = open_owned_log(&root.join(FILE), None, &parent, false)?;
    pins.extend(log_pins);
    let log_id = handle_identity(&log)?;
    let bytes = read_bounded(&mut log, LIMIT)?;
    let mut pending = None;
    let mut count = 0;
    for entry in fs::read_dir(&root).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(INTENT_PREFIX)
        {
            continue;
        }
        count += 1;
        if count > 4096 {
            return Err("PROFILE_SELECTION_FULL: intent inventory bound exceeded".into());
        }
        let (mut file, file_pins) = open_owned_log(&entry.path(), None, &parent, false)?;
        let intent: Intent =
            serde_json::from_slice(&read_bounded(&mut file, METADATA_LIMIT)?).map_err(storage)?;
        if Uuid::parse_str(&intent.request.id).is_err()
            || !matches!(
                intent.request.disposition.as_str(),
                "saved" | "recovery-copy" | "discard"
            )
            || [&intent.selected]
                .into_iter()
                .chain(
                    intent
                        .authorization
                        .as_ref()
                        .and_then(|auth| auth.origin.as_ref()),
                )
                .any(|selected| {
                    Uuid::parse_str(&selected.profile_id).is_err()
                        || !selected.data_root.is_absolute()
                        || selected.data_root.as_os_str().len() > 32768
                })
        {
            return Err(
                "PROFILE_SELECTION_DAMAGED: pending request has invalid native scope".into(),
            );
        }
        if intent.version != 1
            || intent.identity != handle_identity(&file)?
            || intent.parent_identity != parent
            || intent.log_identity != log_id
            || entry.path() != intent_path(&root, &intent.request)
            || intent.request.authorization_digest
                != intent.authorization.as_ref().map(digest).transpose()?
        {
            return Err(
                "PROFILE_SELECTION_IDENTITY_CHANGED: pending request proof is inconsistent".into(),
            );
        }
        let prefix =
            usize::try_from(intent.prior_length).map_err(|_| "PROFILE_SELECTION_DAMAGED")?;
        if prefix > bytes.len() {
            return Err("PROFILE_SELECTION_RECOVERY_REQUIRED: pending prefix is missing".into());
        }
        let (sequence, previous, origin, records) =
            decode_bytes(&bytes[..prefix], &log_id, &parent)?;
        let record = Record {
            sequence: intent.sequence,
            previous: intent.previous.clone(),
            selected: intent.selected.clone(),
            checksum: intent.checksum.clone(),
            request: Some(intent.request.clone()),
        };
        if sequence.checked_add(1) != Some(intent.sequence)
            || previous != intent.previous
            || checksum(&record)? != intent.checksum
        {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: pending request prefix changed".into(),
            );
        }
        let mut intended = serde_json::to_vec(&record).map_err(storage)?;
        intended.push(b'\n');
        let suffix = &bytes[prefix..];
        if suffix.starts_with(&intended) {
            continue;
        }
        if !intended.starts_with(suffix) || pending.is_some() {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: ambiguous pending request suffix".into(),
            );
        }
        validate_intents(&root, &parent, &log_id, &records, Some(&intent.request.id))?;
        pins.extend(file_pins);
        pins.push(file);
        let origin = intent
            .authorization
            .as_ref()
            .and_then(|auth| auth.origin.clone())
            .or(origin);
        pending = Some((intent, origin));
    }
    Ok(pending.map(|(intent, origin)| PendingSelection {
        token: intent.request.id,
        disposition: intent.request.disposition,
        selected: intent.selected,
        authorization: intent.authorization,
        origin,
        _pins: pins,
        _log: log,
    }))
}
