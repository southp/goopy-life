//! Waiting for a freshly provisioned instance to actually serve HTTP.
//!
//! Provisioning is a sequence of steps that return long before the service
//! behind them is usable: a `Type=simple` systemd unit counts as active the
//! instant `ExecStart` has forked, and Ghost binds its port immediately and
//! then answers its own maintenance page for the whole of first boot —
//! migrations against a brand-new SQLite file, fixture seeding, theme
//! compilation. Handing the URL over at that point shows the visitor a broken
//! page and offers them nothing but the reload button (#151).
//!
//! So a provisioner blocks here until the instance answers, and only then
//! reports success. `GoopyManager::spawn` already runs provisioning on its own
//! thread, so this costs the HTTP handler nothing — the frontend simply keeps
//! showing `Spawning…` for the extra seconds, which is what is actually true.
//!
//! Shared rather than Ghost-specific: `HelloProvisioner` has the same race in
//! miniature, and waking a suspended instance (#96) is the same question asked
//! again.

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::shared_types::Error;
use crate::sys_utils::SysRunner;

/// What the probe asks for: the instance's front page, which is exactly what
/// the visitor is about to open.
const READY_PATH: &str = "/";

/// The only status that means "serving".
///
/// Deliberately an allowlist of one. A booting Ghost answers `503`, but the
/// rule does not depend on knowing that: anything other than a real `200` —
/// another 5xx, a redirect, a reply that is not HTTP at all — is a page the
/// visitor should not be shown yet.
const READY_STATUS: u16 = 200;

/// Default ceiling on how long an instance may take to serve.
///
/// One measurement of one Ghost booting alone on the dev droplet took ~13 s
/// (#151). The budget is an order of magnitude above that because a host
/// running several instances, or one competing for IO, is slow rather than
/// broken — and the cost of waiting too long is a spinner, while the cost of
/// giving up too early is a sandbox thrown away seconds before it worked.
pub(crate) const DEFAULT_READY_TIMEOUT_SECS: u64 = 120;

/// Default gap between probes. Short enough that a fast boot is handed over
/// promptly, long enough that a slow one costs a couple of hundred requests
/// rather than thousands.
pub(crate) const DEFAULT_READY_POLL_MS: u64 = 500;

/// How long to wait for an instance, and how often to ask.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadinessBudget {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

/// Blocks until the instance on `port` answers `GET /` with `200`.
///
/// Probes `127.0.0.1:{port}` directly, never the public `{slug}.{domain}` URL:
/// that one goes through TLS and the nginx `auth_request` gate, which denies
/// anything not yet `Done` — and the instance cannot reach `Done` until this
/// call returns. Probing the public URL would deadlock.
///
/// Returns [`Error::ReadinessTimeout`] once the budget is spent, carrying the
/// last thing the instance said. The caller treats that as a failed
/// provision — an instance that never booted is exactly that.
pub(crate) fn wait_until_ready(
    sys: &dyn SysRunner,
    slug: &str,
    port: u32,
    budget: ReadinessBudget,
) -> Result<(), Error> {
    let addr = format!("127.0.0.1:{port}");
    let started = Instant::now();
    let deadline = started + budget.timeout;
    let mut attempts = 0_u32;

    loop {
        attempts += 1;
        let last = match sys.http_probe(&addr, READY_PATH) {
            Ok(READY_STATUS) => {
                info!(
                    slug,
                    %addr,
                    attempts,
                    waited_ms = started.elapsed().as_millis(),
                    "instance is serving"
                );
                return Ok(());
            }
            Ok(code) => format!("status {code}"),
            Err(e) => format!("no HTTP answer: {e}"),
        };

        let now = Instant::now();
        if now >= deadline {
            warn!(slug, %addr, attempts, %last, "instance never became ready");
            return Err(Error::ReadinessTimeout {
                slug: slug.to_string(),
                waited_secs: budget.timeout.as_secs(),
                last,
            });
        }

        debug!(slug, %addr, attempts, %last, "instance not ready yet, waiting");
        // Never sleep past the deadline: the budget is what the operator
        // configured, not that value rounded up to a poll interval.
        std::thread::sleep(budget.poll_interval.min(deadline - now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys_utils::{MockProbe, MockSysRunner};

    /// A budget short enough that the timeout case is a fast test, with a poll
    /// interval that still leaves room for several attempts.
    fn quick_budget() -> ReadinessBudget {
        ReadinessBudget {
            timeout: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
        }
    }

    #[test]
    fn returns_as_soon_as_the_instance_serves() {
        let sys = MockSysRunner::with_probes(vec![MockProbe::Status(200)]);

        wait_until_ready(&sys, "tasty-lucky-clover", 9876, quick_budget())
            .expect("a serving instance is ready");

        assert_eq!(
            sys.http_probes().len(),
            1,
            "an instance that is already serving must not be polled twice"
        );
    }

    /// The case the whole module exists for: Ghost is listening from the first
    /// millisecond and answers its maintenance page until it has finished
    /// booting.
    #[test]
    fn keeps_polling_through_not_ready_answers() {
        let sys = MockSysRunner::with_probes(vec![
            MockProbe::Unreachable,
            MockProbe::Status(503),
            MockProbe::Status(503),
            MockProbe::Status(200),
        ]);

        wait_until_ready(&sys, "tasty-lucky-clover", 9876, quick_budget())
            .expect("an instance that boots within the budget is ready");

        assert_eq!(
            sys.http_probes().len(),
            4,
            "it should have kept asking until the 200"
        );
    }

    #[test]
    fn probes_the_instance_port_directly_rather_than_the_public_url() {
        let sys = MockSysRunner::new();

        wait_until_ready(&sys, "tasty-lucky-clover", 9876, quick_budget()).unwrap();

        assert_eq!(
            sys.http_probes(),
            [("127.0.0.1:9876".to_string(), "/".to_string())],
            "the public URL cannot answer until this call returns, so probing \
             it would deadlock"
        );
    }

    #[test]
    fn gives_up_once_the_budget_is_spent_and_reports_the_last_answer() {
        let sys = MockSysRunner::with_probes(vec![MockProbe::Status(503)]);

        let err = wait_until_ready(&sys, "tasty-lucky-clover", 9876, quick_budget())
            .expect_err("an instance that never boots must not be handed over");

        match err {
            Error::ReadinessTimeout { slug, last, .. } => {
                assert_eq!(slug, "tasty-lucky-clover");
                assert!(
                    last.contains("503"),
                    "the last observation is what #118 has to record, got {last:?}"
                );
            }
            other => panic!("expected a readiness timeout, got {other:?}"),
        }
        assert!(
            sys.http_probes().len() > 1,
            "the budget should buy more than a single attempt"
        );
    }

    #[test]
    fn a_response_that_is_not_200_is_never_ready() {
        // A redirect is a plausible thing for a half-configured instance to
        // answer, and it is not a page the visitor should be shown.
        let sys = MockSysRunner::with_probes(vec![MockProbe::Status(302)]);

        let err = wait_until_ready(&sys, "tasty-lucky-clover", 9876, quick_budget())
            .expect_err("only a real 200 counts as serving");

        assert!(matches!(err, Error::ReadinessTimeout { .. }), "got {err:?}");
    }

    #[test]
    fn does_not_wait_longer_than_its_budget() {
        let sys = MockSysRunner::with_probes(vec![MockProbe::Unreachable]);
        let budget = ReadinessBudget {
            timeout: Duration::from_millis(30),
            poll_interval: Duration::from_millis(100),
        };

        let started = Instant::now();
        wait_until_ready(&sys, "tasty-lucky-clover", 9876, budget).unwrap_err();

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a poll interval longer than the remaining budget must be clamped, \
             not slept through"
        );
    }
}
