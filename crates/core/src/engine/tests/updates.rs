//! Engine tests: Klefki's own updates.
use super::*;

fn update_found(core: &Core) -> crate::update::UpdateCheck {
    core.send(Command::CheckForUpdates);
    let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Update(_))));
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Update(found) => Some(found),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_newer_signed_release_is_reported_and_its_page_opened_on_request() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let (releases, key) = crate::update::test_release();
    let core = start_with(
        EngineConfig {
            releases,
            release_key: Some(key),
            ..env.config()
        },
        || {},
    );
    assert!(core.checks_for_updates());
    let version = crate::update::Version::parse("0.2.0").unwrap();
    assert_eq!(
        update_found(&core),
        crate::update::UpdateCheck::Available {
            version,
            page: "https://github.com/DavySc/Klefki/releases/tag/v0.2.0".into(),
        }
    );
    core.send(Command::OpenReleasePage(version));
    // Commands are handled in order, so once this check answers, the page was opened.
    update_found(&core);
    assert_eq!(
        *lock(&env.opened),
        ["https://github.com/DavySc/Klefki/releases/tag/v0.2.0"]
    );
}

fn downloaded(core: &Core) -> UpdateDownload {
    core.send(Command::DownloadUpdate);
    let events = collect(core, |e| {
        e.iter().any(|x| matches!(x, Event::UpdateDownload(_)))
    });
    events
        .into_iter()
        .find_map(|e| match e {
            Event::UpdateDownload(done) => Some(done),
            _ => None,
        })
        .unwrap()
}

#[test]
fn the_update_a_check_found_is_downloaded_and_checked_for_this_install() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let (releases, key) = crate::update::test_release();
    let core = start_with(
        EngineConfig {
            releases,
            release_key: Some(key),
            install: crate::install::InstallKind::WindowsSetup,
            ..env.config()
        },
        || {},
    );
    assert!(
        matches!(downloaded(&core), UpdateDownload::Failed(why) if why.contains("No newer release")),
        "nothing to download before a check found it"
    );
    assert!(matches!(
        update_found(&core),
        crate::update::UpdateCheck::Available { .. }
    ));
    let version = crate::update::Version::parse("0.2.0").unwrap();
    assert_eq!(downloaded(&core), UpdateDownload::Ready(version));
    let file = env
        .dir
        .path()
        .join("updates/klefki-0.2.0-windows-x64-setup.exe");
    assert_eq!(std::fs::read(&file).unwrap(), b"b");
    // Asked again, the download is not repeated.
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert_eq!(downloaded(&core), UpdateDownload::Ready(version));
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn an_update_that_failed_as_klefki_closed_is_reported_by_the_next_check() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    crate::install::note_failure(
        &env.dir.path().join("updates"),
        "Could not replace it (denied).",
    );
    let (releases, key) = crate::update::test_release();
    let core = start_with(
        EngineConfig {
            releases,
            release_key: Some(key),
            ..env.config()
        },
        || {},
    );
    core.send(Command::CheckForUpdates);
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Update(_))));
    let failed = events.iter().position(|e| {
        matches!(e, Event::UpdateDownload(UpdateDownload::Failed(why))
            if why == "The update could not be installed when Klefki closed: Could not replace it (denied).")
    });
    let answer = events.iter().position(|e| matches!(e, Event::Update(_)));
    assert!(
        failed.unwrap() < answer.unwrap(),
        "told before the answer: {events:?}"
    );
    // Once.
    update_found(&core);
    assert!(!env.dir.path().join("updates/install-failed.txt").exists());
}

#[test]
fn an_install_updated_by_hand_downloads_nothing() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let (releases, key) = crate::update::test_release();
    let core = start_with(
        EngineConfig {
            releases,
            release_key: Some(key),
            ..env.config()
        },
        || {},
    );
    update_found(&core);
    assert_eq!(
        downloaded(&core),
        UpdateDownload::Manual("Updated by hand in tests.".into())
    );
    assert!(!env.dir.path().join("updates").exists());
    assert_eq!(
        core.install_update(false),
        Err("No update has been downloaded.".into())
    );
}

#[test]
fn without_a_release_key_no_update_check_reaches_out() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let core = env.start();
    assert!(!core.checks_for_updates());
    assert_eq!(update_found(&core), crate::update::UpdateCheck::NoKey);
}
