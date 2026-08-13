#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    Unchanged,
    UploadLocal,
    DownloadRemote,
    Converged,
    Conflict,
    FirstSyncChoiceRequired,
}

/// Three-way comparison. Timestamps are deliberately absent: hashes alone
/// determine identity and BASE is the last version verified on both sides.
pub fn decide(base: Option<&str>, local: &str, remote: &str) -> SyncDecision {
    let Some(base) = base else {
        return if local == remote {
            SyncDecision::Converged
        } else {
            SyncDecision::FirstSyncChoiceRequired
        };
    };

    match (local == base, remote == base, local == remote) {
        (true, true, _) => SyncDecision::Unchanged,
        (false, true, _) => SyncDecision::UploadLocal,
        (true, false, _) => SyncDecision::DownloadRemote,
        (false, false, true) => SyncDecision::Converged,
        (false, false, false) => SyncDecision::Conflict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implements_three_way_matrix() {
        assert_eq!(decide(Some("a"), "a", "a"), SyncDecision::Unchanged);
        assert_eq!(decide(Some("a"), "b", "a"), SyncDecision::UploadLocal);
        assert_eq!(decide(Some("a"), "a", "b"), SyncDecision::DownloadRemote);
        assert_eq!(decide(Some("a"), "b", "b"), SyncDecision::Converged);
        assert_eq!(decide(Some("a"), "b", "c"), SyncDecision::Conflict);
    }

    #[test]
    fn first_sync_never_guesses() {
        assert_eq!(
            decide(None, "a", "b"),
            SyncDecision::FirstSyncChoiceRequired
        );
        assert_eq!(decide(None, "a", "a"), SyncDecision::Converged);
    }
}
