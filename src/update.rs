//! Tells the user when a newer release exists, without getting in the way.
//!
//! The notice is worth nothing if it costs the user anything, so it is built
//! to be nearly free: at most one request a day, answered by a redirect that
//! carries no body, started before the command runs and read after it, so a
//! command that talks to the API waits for nothing it would not have waited
//! for anyway. What the request found is cached beside the configuration file
//! and every other run of that day is answered from there without touching the
//! network at all.

use std::io::{self, IsTerminal as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::output::style;

/// The release this binary was built from. Unstamped builds report
/// `0.0.0-dev`, which [`precedence`] refuses to order, so a checkout build
/// never nags its own developer.
const CURRENT: &str = env!("BRAINPOD_VERSION");
const RELEASES_URL: &str = "https://github.com/brainpodnl/cli/releases/latest";
const CACHE_FILE: &str = "version-check.json";
const INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);
/// How long an attempt that never came back suppresses the next one. Short,
/// because the request is abandoned whenever the command outruns it, and a
/// full day of silence for a user whose link is slower than [`GRACE`] would
/// mean never hearing about a release at all.
const RETRY: Duration = Duration::from_secs(60 * 60);
/// Generous, because nothing waits on it: the answer is a redirect with an
/// empty body, so a slow one means a slow network rather than a big download.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// What a notice is worth delaying the user's prompt by once their command is
/// done. A request still in flight is abandoned and the next run reads what
/// this one was about to.
const GRACE: Duration = Duration::from_millis(250);

/// A check in flight, plus whatever the last one found.
pub struct Check {
    known: Option<String>,
    fetch: Option<JoinHandle<Option<String>>>,
}

/// Starts the check, if this run is one that should carry a notice at all.
///
/// Silent for `--json` and for anything that is not a terminal: those runs are
/// read by programs, which have no use for the notice and should not pay for
/// the request behind it.
pub fn start(json: bool) -> Check {
    if json || suppressed() || !io::stderr().is_terminal() {
        return Check::quiet();
    }

    let Ok(path) = cache_path() else {
        return Check::quiet();
    };
    let cache = read(&path);
    let known = cache.as_ref().and_then(|cache| cache.latest.clone());
    if !due(cache.as_ref(), now()) {
        return Check { known, fetch: None };
    }

    // Stamped before the request rather than after it, so a run that ends
    // before the answer arrives, or a machine with no route to GitHub, cannot
    // turn into one request per command. The attempt is recorded as
    // unanswered, which is what [`due`] retries on the shorter interval.
    if !write(
        &path,
        &Cache {
            checked_at: now(),
            latest: known.clone(),
            unanswered: true,
        },
    ) {
        // Nowhere to record it. A check that cannot be cached would run on
        // every single command, so this run says only what it already knew.
        return Check { known, fetch: None };
    }

    let fetch = tokio::spawn(async move {
        let latest = fetch_latest().await?;
        write(
            &path,
            &Cache {
                checked_at: now(),
                latest: Some(latest.clone()),
                unanswered: false,
            },
        );
        Some(latest)
    });

    Check {
        known,
        fetch: Some(fetch),
    }
}

impl Check {
    const fn quiet() -> Self {
        Self {
            known: None,
            fetch: None,
        }
    }

    /// Reports on the newest release this run knows about. Call it after the
    /// command has written its own output: the notice belongs underneath the
    /// answer the user asked for, never in the middle of it.
    pub async fn notify(self) {
        let mut latest = self.known;
        // A request still running is left to finish into the cache on its own;
        // its answer is this run's only if it is already here.
        if let Some(fetch) = self.fetch
            && let Ok(Ok(Some(fetched))) = tokio::time::timeout(GRACE, fetch).await
        {
            latest = Some(fetched);
        }

        let Some(latest) = latest else {
            return;
        };
        if let Some(notice) = notice(CURRENT, &latest, io::stderr().is_terminal()) {
            eprint!("{notice}");
        }
    }
}

/// `BRAINPOD_NO_UPDATE_CHECK` turns the notice off; `CI` is honoured because a
/// build machine cannot act on it and its terminal is often a pipe that looks
/// interactive.
fn suppressed() -> bool {
    ["BRAINPOD_NO_UPDATE_CHECK", "CI"]
        .iter()
        .any(|name| crate::environment(name).is_some_and(|value| value != "0"))
}

/// Asks GitHub which release is current by reading where `latest` points.
///
/// The redirect is the whole answer: one request, no body, no API token and no
/// rate limit worth the name, where the releases API would return the entire
/// release including its notes and every asset.
async fn fetch_latest() -> Option<String> {
    let http = crate::http_client_builder()
        .ok()?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("brainpod/", env!("BRAINPOD_VERSION")))
        .build()
        .ok()?;
    let response = http.get(RELEASES_URL).send().await.ok()?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    tag_version(location)
}

/// The tag the `latest` alias redirects to, as a version.
fn tag_version(location: &str) -> Option<String> {
    let tag = location
        .split(['?', '#'])
        .next()?
        .rsplit('/')
        .find(|segment| !segment.is_empty())?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    precedence(version).map(|_| version.to_owned())
}

fn notice(current: &str, latest: &str, color: bool) -> Option<String> {
    if !is_newer(latest, current) {
        return None;
    }

    Some(format!(
        "\n{}\nNew version {latest} is available, you are on {current}. Download: {}\n",
        style("Update Available", "1;33", color),
        style(RELEASES_URL, "36", color)
    ))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (precedence(latest), precedence(current)) {
        (Some(latest), Some(current)) => latest > current,
        // A version neither side can read is a build nobody released: say
        // nothing rather than nag a developer running their own checkout.
        _ => false,
    }
}

/// Plain `major.minor.patch` only. Pre-release and build metadata are rejected
/// rather than ordered, because a comparison this notice cannot get right is
/// one it should not make.
fn precedence(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cache {
    checked_at: u64,
    latest: Option<String>,
    /// An attempt was started and nothing came back from it. Absent in a file
    /// written before this field existed, which reads as answered: whatever
    /// wrote it did have a version to record.
    #[serde(default)]
    unanswered: bool,
}

fn due(cache: Option<&Cache>, now: u64) -> bool {
    let Some(cache) = cache else {
        return true;
    };
    // A stamp from the future is a clock that moved, not a check that ran, and
    // waiting it out would mute the notice until the clock catches up.
    if cache.checked_at > now {
        return true;
    }

    let interval = if cache.unanswered { RETRY } else { INTERVAL };
    now - cache.checked_at >= interval.as_secs()
}

fn cache_path() -> Result<PathBuf> {
    Ok(Config::path()?.with_file_name(CACHE_FILE))
}

fn read(path: &Path) -> Option<Cache> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Reports whether the record survived, which is what keeps a check from
/// running on every command when there is nowhere to remember it: a read-only
/// home directory or a full disk must never fail the command the user ran, and
/// must never be answered by asking GitHub again instead. A half-written file
/// fails to parse and costs one extra request, which is why it is not worth
/// replacing through a temporary file.
fn write(path: &Path, cache: &Cache) -> bool {
    let Ok(contents) = serde_json::to_vec(cache) else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, contents).is_ok()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_release_off_the_redirect() {
        assert_eq!(
            tag_version("https://github.com/brainpodnl/cli/releases/tag/v0.0.5").as_deref(),
            Some("0.0.5")
        );
        assert_eq!(
            tag_version("https://github.com/brainpodnl/cli/releases/tag/1.2.3/").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn ignores_a_redirect_that_names_no_release() {
        assert_eq!(
            tag_version("https://github.com/brainpodnl/cli/releases"),
            None
        );
        assert_eq!(
            tag_version("https://github.com/brainpodnl/cli/releases/tag/nightly"),
            None
        );
        assert_eq!(tag_version(""), None);
    }

    #[test]
    fn orders_releases_by_precedence() {
        assert!(is_newer("0.0.6", "0.0.5"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("1.0.0", "0.42.7"));
        assert!(!is_newer("0.0.5", "0.0.5"));
        assert!(!is_newer("0.0.4", "0.0.5"));
        assert!(!is_newer("0.10.0", "0.10.1"));
    }

    #[test]
    fn stays_quiet_about_versions_it_cannot_order() {
        assert!(!is_newer("0.0.6-rc.1", "0.0.5"));
        assert!(!is_newer("0.0.6", "0.0.5-dev"));
        assert!(!is_newer("0.0.6.1", "0.0.5"));
        assert!(!is_newer("main", "0.0.5"));
    }

    #[test]
    fn announces_only_a_newer_release() {
        let announcement = notice("0.0.5", "0.0.6", false).unwrap();
        assert!(announcement.contains("New version 0.0.6 is available, you are on 0.0.5."));
        assert!(announcement.contains(RELEASES_URL));
        assert!(!announcement.contains('\u{1b}'));

        assert_eq!(notice("0.0.5", "0.0.5", false), None);
        assert_eq!(notice("0.0.6", "0.0.5", false), None);
    }

    #[test]
    fn colors_the_notice_for_a_terminal() {
        let announcement = notice("0.0.5", "0.0.6", true).unwrap();
        assert!(announcement.contains("\u{1b}[1;33mUpdate Available\u{1b}[0m"));
    }

    #[test]
    fn checks_once_a_day() {
        let now = 1_700_000_000;
        let answered = |seconds_ago: u64| Cache {
            checked_at: now - seconds_ago,
            latest: Some("0.0.5".to_owned()),
            unanswered: false,
        };

        assert!(due(None, now));
        assert!(due(Some(&answered(INTERVAL.as_secs())), now));
        assert!(!due(Some(&answered(0)), now));
        assert!(!due(Some(&answered(INTERVAL.as_secs() - 1)), now));
    }

    #[test]
    fn retries_an_attempt_that_never_answered() {
        let now = 1_700_000_000;
        let unanswered = |seconds_ago: u64| Cache {
            checked_at: now - seconds_ago,
            latest: None,
            unanswered: true,
        };

        assert!(!due(Some(&unanswered(RETRY.as_secs() - 1)), now));
        assert!(due(Some(&unanswered(RETRY.as_secs())), now));
        // Far short of a day: a link slower than the grace period would
        // otherwise never resolve a single check.
        assert!(due(Some(&unanswered(INTERVAL.as_secs() / 2)), now));
    }

    #[test]
    fn checks_again_when_the_clock_moved_back() {
        let now = 1_700_000_000;
        assert!(due(
            Some(&Cache {
                checked_at: now + 60,
                latest: Some("0.0.5".to_owned()),
                unanswered: false,
            }),
            now
        ));
    }

    #[test]
    fn keeps_the_result_between_runs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join(CACHE_FILE);

        assert!(read(&path).is_none());
        assert!(write(
            &path,
            &Cache {
                checked_at: 42,
                latest: Some("0.0.6".to_owned()),
                unanswered: false,
            }
        ));

        let cache = read(&path).unwrap();
        assert_eq!(cache.checked_at, 42);
        assert_eq!(cache.latest.as_deref(), Some("0.0.6"));
        assert!(!cache.unanswered);
    }

    #[test]
    fn reports_a_cache_it_could_not_write() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("config.toml");
        std::fs::write(&blocked, b"").unwrap();

        assert!(!write(&blocked.join(CACHE_FILE), &Cache::default()));
    }

    #[test]
    fn reads_a_cache_written_before_attempts_were_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CACHE_FILE);
        std::fs::write(&path, br#"{"checkedAt":1700000000,"latest":"0.0.5"}"#).unwrap();

        let cache = read(&path).unwrap();
        assert_eq!(cache.latest.as_deref(), Some("0.0.5"));
        assert!(!cache.unanswered);
        assert!(!due(Some(&cache), 1_700_000_001));
    }

    #[test]
    fn survives_a_cache_written_by_something_else() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CACHE_FILE);
        std::fs::write(&path, b"{\"checkedAt\":").unwrap();

        assert!(read(&path).is_none());
        assert!(due(read(&path).as_ref(), now()));
    }
}
