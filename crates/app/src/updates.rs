//! Klefki's own updates as the user sees them (decision 009): a check in the background once a day, never while
//! Klefki starts, what it found, and the download of it. The engine does the checking and downloading; this only
//! decides when to ask and remembers the answer between runs.

use klefki_core::api::{UpdateCheck, UpdateDownload, Version};

const AUTO_KEY: &str = "updates.auto";
const LAST_KEY: &str = "updates.last_checked";
const AVAILABLE_KEY: &str = "updates.available";

/// How long after the window first shows the automatic check waits, so starting never competes with it.
pub const START_DELAY: f64 = 3.0;
/// The automatic check runs at most this often, in seconds.
pub const EVERY: u64 = 24 * 60 * 60;

pub struct Updates {
    /// Klefki looks for updates by itself. On unless the user turned it off.
    pub auto: bool,
    /// When a check last got an answer, in seconds since 1970.
    last_checked: Option<u64>,
    /// A newer release found by an earlier check, until a check finds none.
    available: Option<Version>,
    /// What the check this run found, or why it failed.
    pub found: Option<UpdateCheck>,
    pub checking: bool,
    /// The automatic check of this run was considered (run or not), so it is not considered again.
    considered: bool,
    /// What the download of the found release did, this run.
    pub download: Option<UpdateDownload>,
    pub downloading: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            auto: true,
            last_checked: None,
            available: None,
            found: None,
            checking: false,
            considered: false,
            download: None,
            downloading: false,
        }
    }
}

impl Updates {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Updates {
        let get = |key| storage.and_then(|s| s.get_string(key));
        Updates {
            auto: get(AUTO_KEY).is_none_or(|v| v != "off"),
            last_checked: get(LAST_KEY).and_then(|v| v.parse().ok()),
            available: get(AVAILABLE_KEY)
                .and_then(|v| Version::parse(&v))
                .filter(|v| *v > Version::current()),
            ..Updates::default()
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string(AUTO_KEY, if self.auto { "on" } else { "off" }.to_owned());
        match self.last_checked {
            Some(at) => storage.set_string(LAST_KEY, at.to_string()),
            None => storage.remove_string(LAST_KEY),
        }
        match self.available {
            Some(version) => storage.set_string(AVAILABLE_KEY, version.to_string()),
            None => storage.remove_string(AVAILABLE_KEY),
        }
    }

    /// Whether the automatic check should start now. `uptime` is how long the window has been open and `now` the
    /// time, both in seconds. Answers yes at most once a run, and not again within [`EVERY`] of the last answer.
    pub fn due(&mut self, uptime: f64, now: u64) -> bool {
        if !self.auto || self.considered || self.checking || uptime < START_DELAY {
            return false;
        }
        self.considered = true;
        // A last check in the future means the clock was changed: check rather than wait. A release found earlier is
        // looked up again at once, so it can be downloaded this run.
        let recent = self.available.is_none()
            && self
                .last_checked
                .is_some_and(|at| at <= now && now - at < EVERY);
        if recent {
            return false;
        }
        self.checking = true;
        true
    }

    /// The user asked to check now.
    pub fn check_now(&mut self) {
        self.checking = true;
    }

    pub fn answered(&mut self, found: UpdateCheck, now: u64) {
        self.checking = false;
        match &found {
            UpdateCheck::Available { version, .. } => {
                if self.ready() != Some(*version) {
                    self.download = None;
                }
                self.available = Some(*version);
                self.last_checked = Some(now);
            }
            UpdateCheck::UpToDate | UpdateCheck::NoRelease => {
                self.available = None;
                self.last_checked = Some(now);
            }
            // Tried again on the next start: a failure is not an answer.
            UpdateCheck::Failed(_) | UpdateCheck::NoKey => {}
        }
        self.found = Some(found);
    }

    /// The download should start now: a release was found and [`Updates::auto`] is on, and this run has not tried yet.
    pub fn download_due(&self) -> bool {
        self.auto && self.available.is_some() && self.download.is_none() && !self.downloading
    }

    /// The download was asked for.
    pub fn downloading(&mut self) {
        self.downloading = true;
    }

    pub fn downloaded(&mut self, done: UpdateDownload) {
        self.downloading = false;
        self.download = Some(done);
    }

    /// A version downloaded and checked this run, ready to install.
    pub fn ready(&self) -> Option<Version> {
        match self.download {
            Some(UpdateDownload::Ready(version)) => Some(version),
            _ => None,
        }
    }

    /// The download failed; why.
    pub fn download_failed(&self) -> Option<&str> {
        match &self.download {
            Some(UpdateDownload::Failed(why)) => Some(why),
            _ => None,
        }
    }

    /// A newer release, found this run or by an earlier one.
    pub fn available(&self) -> Option<Version> {
        self.available
    }

    /// "Up to date · checked …", when the last answer, this run or an earlier one, found nothing newer.
    pub fn up_to_date(&self, now: u64) -> Option<String> {
        if self.available.is_some() {
            return None;
        }
        self.last_checked
            .map(|at| format!("Up to date · checked {}", ago(at, now)))
    }
}

/// How long ago `at` was, in words: "just now", "3 hours ago", "yesterday", "5 days ago".
fn ago(at: u64, now: u64) -> String {
    const HOUR: u64 = 60 * 60;
    let elapsed = now.saturating_sub(at);
    match elapsed {
        e if e < HOUR => "just now".into(),
        e if e < 2 * HOUR => "an hour ago".into(),
        e if e < EVERY => format!("{} hours ago", e / HOUR),
        e if e < 2 * EVERY => "yesterday".into(),
        e => format!("{} days ago", e / EVERY),
    }
}

/// The time now, in seconds since 1970; 0 if the clock is before that.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::tests::MemoryStorage;

    const DAY: u64 = EVERY;
    const NOW: u64 = 1_800_000_000;

    fn newer() -> Version {
        Version::parse("999.0.0").unwrap()
    }

    #[test]
    fn checks_once_after_the_start_delay_on_a_first_run() {
        let mut updates = Updates::default();
        assert!(!updates.due(0.0, NOW), "not while starting");
        assert!(!updates.due(START_DELAY - 0.1, NOW));
        assert!(updates.due(START_DELAY, NOW));
        assert!(updates.checking);
        updates.answered(UpdateCheck::UpToDate, NOW);
        assert!(
            !updates.due(START_DELAY + 100.0, NOW + 2 * DAY),
            "once a run"
        );
    }

    #[test]
    fn waits_a_day_after_the_last_answer() {
        let mut storage = MemoryStorage::default();
        let mut first = Updates::default();
        assert!(first.due(START_DELAY, NOW));
        first.answered(UpdateCheck::UpToDate, NOW);
        first.save(&mut storage);

        assert!(!Updates::load(Some(&storage)).due(START_DELAY, NOW + DAY - 1));
        assert!(Updates::load(Some(&storage)).due(START_DELAY, NOW + DAY));
        // The clock was set back: a last check "in the future" does not stop checking for good.
        assert!(Updates::load(Some(&storage)).due(START_DELAY, NOW - 10));
    }

    #[test]
    fn a_failed_check_is_tried_again_next_start() {
        let mut storage = MemoryStorage::default();
        let mut first = Updates::default();
        assert!(first.due(START_DELAY, NOW));
        first.answered(UpdateCheck::Failed("no connection".into()), NOW);
        assert!(!first.checking);
        first.save(&mut storage);
        assert!(Updates::load(Some(&storage)).due(START_DELAY, NOW + 60));
    }

    #[test]
    fn turned_off_it_never_checks_by_itself_and_stays_off() {
        let mut storage = MemoryStorage::default();
        let mut updates = Updates {
            auto: false,
            ..Updates::default()
        };
        assert!(!updates.due(START_DELAY, NOW));
        updates.save(&mut storage);
        let mut again = Updates::load(Some(&storage));
        assert!(!again.auto);
        assert!(!again.due(1000.0, NOW + 10 * DAY));
        // Asking still works.
        again.check_now();
        assert!(again.checking);
    }

    #[test]
    fn a_found_release_is_remembered_until_a_check_finds_none() {
        let mut storage = MemoryStorage::default();
        let mut updates = Updates::default();
        updates.answered(
            UpdateCheck::Available {
                version: newer(),
                page: String::new(),
            },
            NOW,
        );
        updates.save(&mut storage);
        let mut later = Updates::load(Some(&storage));
        assert_eq!(later.available(), Some(newer()));
        later.answered(UpdateCheck::Failed("no connection".into()), NOW + 1);
        assert_eq!(
            later.available(),
            Some(newer()),
            "a failure does not forget it"
        );
        later.answered(UpdateCheck::UpToDate, NOW + 2);
        later.save(&mut storage);
        assert_eq!(Updates::load(Some(&storage)).available(), None);
    }

    #[test]
    fn a_remembered_release_that_is_not_newer_is_dropped() {
        // After installing the update by hand, the remembered version is this one: nothing to offer.
        let mut storage = MemoryStorage::default();
        eframe::Storage::set_string(&mut storage, AVAILABLE_KEY, Version::current().to_string());
        assert_eq!(Updates::load(Some(&storage)).available(), None);
        eframe::Storage::set_string(&mut storage, AVAILABLE_KEY, "garbage".into());
        assert_eq!(Updates::load(Some(&storage)).available(), None);
    }

    #[test]
    fn says_when_it_last_found_nothing_newer() {
        let mut storage = MemoryStorage::default();
        let mut updates = Updates::default();
        assert_eq!(updates.up_to_date(NOW), None, "never checked");
        updates.answered(UpdateCheck::UpToDate, NOW);
        updates.save(&mut storage);
        // Shown on a later start too, from what was saved.
        let later = Updates::load(Some(&storage));
        assert_eq!(
            later.up_to_date(NOW + 60).as_deref(),
            Some("Up to date · checked just now")
        );
        assert_eq!(
            later.up_to_date(NOW + 3 * 3600).as_deref(),
            Some("Up to date · checked 3 hours ago")
        );
        assert_eq!(
            later.up_to_date(NOW + DAY + 5).as_deref(),
            Some("Up to date · checked yesterday")
        );
        assert_eq!(
            later.up_to_date(NOW + 9 * DAY).as_deref(),
            Some("Up to date · checked 9 days ago")
        );
        // A clock set back does not make it "in the future".
        assert_eq!(
            later.up_to_date(NOW - 100).as_deref(),
            Some("Up to date · checked just now")
        );
        let mut found = later;
        found.answered(
            UpdateCheck::Available {
                version: newer(),
                page: String::new(),
            },
            NOW,
        );
        assert_eq!(found.up_to_date(NOW), None);
    }

    #[test]
    fn no_published_release_is_an_answer() {
        let mut updates = Updates::default();
        assert!(updates.due(START_DELAY, NOW));
        updates.answered(UpdateCheck::NoRelease, NOW);
        assert!(!updates.checking);
        assert!(updates.up_to_date(NOW).is_some());
    }

    #[test]
    fn a_release_found_earlier_is_looked_up_again_at_once_to_download_it() {
        let mut storage = MemoryStorage::default();
        let mut first = Updates::default();
        first.answered(
            UpdateCheck::Available {
                version: newer(),
                page: String::new(),
            },
            NOW,
        );
        first.save(&mut storage);
        assert!(Updates::load(Some(&storage)).due(START_DELAY, NOW + 60));
    }

    #[test]
    fn a_found_release_is_downloaded_once_when_automatic() {
        let mut updates = Updates::default();
        assert!(!updates.download_due(), "nothing found yet");
        let found = UpdateCheck::Available {
            version: newer(),
            page: String::new(),
        };
        updates.answered(found.clone(), NOW);
        assert!(updates.download_due());
        updates.downloading();
        assert!(!updates.download_due(), "not twice at once");
        updates.downloaded(UpdateDownload::Failed("no connection".into()));
        assert!(
            !updates.download_due(),
            "not again by itself after a failure"
        );
        assert_eq!(updates.download_failed(), Some("no connection"));
        updates.downloaded(UpdateDownload::Ready(newer()));
        assert_eq!(updates.ready(), Some(newer()));
        // The same release found again keeps the download.
        updates.answered(found, NOW + 1);
        assert_eq!(updates.ready(), Some(newer()));

        let mut manual = Updates {
            auto: false,
            ..Updates::default()
        };
        manual.answered(
            UpdateCheck::Available {
                version: newer(),
                page: String::new(),
            },
            NOW,
        );
        assert!(!manual.download_due(), "only when automatic");
    }

    #[test]
    fn on_by_default() {
        assert!(Updates::load(None).auto);
        assert!(Updates::load(Some(&MemoryStorage::default())).auto);
    }
}
