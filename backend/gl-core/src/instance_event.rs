//! The record of a failure, in a store that outlives the instance (#118).
//!
//! A `goopies` row carries `status = Failed` and nothing else, and the sweep
//! reaps `Failed` rows — so the row that says *something went wrong* is deleted
//! before anyone asks *what*. Ten such rows wedged the dev droplet at its
//! capacity cap for two and a half months in 2026, and by the time they were
//! looked at there was nothing left to look at: the `tracing::error!` had gone
//! to a journal neither account on the host can read, and had long since
//! rotated.
//!
//! An [`InstanceEvent`] is written where the error is — the failure arm of a
//! spawn, the failure arm of a teardown — into an append-only table with no
//! foreign key to `goopies`. Outliving the row is the entire point.

use chrono::{DateTime, Utc};

use crate::shared_types::Error;

/// Which operation the event came out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EventPhase {
    /// Provisioning a new instance.
    Spawn,
    /// A teardown somebody asked for — the API, or `gl-cli despawn`.
    Despawn,
    /// A teardown the sweeper started, because the instance expired or was
    /// already `Failed`.
    ///
    /// Distinct from [`Despawn`] even though the teardown is identical: "what
    /// did users tear down" and "what did the sweeper reclaim" are different
    /// questions, and only the log can still tell them apart once the row is
    /// gone.
    ///
    /// [`Despawn`]: EventPhase::Despawn
    Sweep,
}

impl std::fmt::Display for EventPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventPhase::Spawn => write!(f, "spawn"),
            EventPhase::Despawn => write!(f, "despawn"),
            EventPhase::Sweep => write!(f, "sweep"),
        }
    }
}

impl std::str::FromStr for EventPhase {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "spawn" => Ok(EventPhase::Spawn),
            "despawn" => Ok(EventPhase::Despawn),
            "sweep" => Ok(EventPhase::Sweep),
            _ => Err(Error::Invalid),
        }
    }
}

/// How the operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EventOutcome {
    /// Something went wrong. The event carries the error's code and rendering.
    Failed,
    /// The instance left the registry. The row is gone; this is what is left of
    /// it.
    Reaped,
}

impl std::fmt::Display for EventOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventOutcome::Failed => write!(f, "failed"),
            EventOutcome::Reaped => write!(f, "reaped"),
        }
    }
}

impl std::str::FromStr for EventOutcome {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "failed" => Ok(EventOutcome::Failed),
            "reaped" => Ok(EventOutcome::Reaped),
            _ => Err(Error::Invalid),
        }
    }
}

/// One line in the story of an instance, kept after the instance is gone.
///
/// The two payload fields are deliberately different in kind:
///
/// - `code` is [`Error::code`] — a short, stable discriminant. It is the field
///   worth grouping by ("Ghost installs fail on `subprocess` 3% of the time"),
///   and the only one safe to publish.
/// - `detail` is the full rendering of the same error, which is operator-only:
///   [`Error::Subprocess`] carries raw command stderr and
///   [`Error::RowParse`] carries raw row values.
///
/// Keeping both is the point. The code answers *what class of thing broke*
/// without a human; the detail is what a human actually needs when one does
/// look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceEvent {
    pub slug: String,
    pub occurred_at: DateTime<Utc>,
    pub phase: EventPhase,
    pub outcome: EventOutcome,
    pub code: String,
    pub detail: Option<String>,
}

impl InstanceEvent {
    /// The `code` of an event that records no error.
    ///
    /// `code` is `NOT NULL` so that every row can be grouped by it without a
    /// `COALESCE`, which means a successful reap needs a value too. `"ok"` is
    /// that value, and it is as much a contract as any [`Error::code`].
    pub const NO_ERROR: &'static str = "ok";

    /// Record a failure, stamped now, carrying both halves of `err`.
    pub fn failed(slug: &str, phase: EventPhase, err: &Error) -> Self {
        Self {
            slug: slug.to_string(),
            occurred_at: Utc::now(),
            phase,
            outcome: EventOutcome::Failed,
            code: err.code().to_string(),
            detail: Some(err.to_string()),
        }
    }

    /// Record that the instance left the registry, stamped now.
    pub fn reaped(slug: &str, phase: EventPhase) -> Self {
        Self {
            slug: slug.to_string(),
            occurred_at: Utc::now(),
            phase,
            outcome: EventOutcome::Reaped,
            code: Self::NO_ERROR.to_string(),
            detail: None,
        }
    }

    /// Say what was being attempted when this happened.
    ///
    /// An error's own rendering says what broke, not what we were doing: a
    /// failed `DELETE FROM allocated_ports` renders as a registry error like
    /// any other. Prefixing keeps that context in `detail` without inventing a
    /// code for every call site.
    #[must_use]
    pub fn during(mut self, what: impl std::fmt::Display) -> Self {
        self.detail = Some(match self.detail {
            Some(detail) => format!("{what}: {detail}"),
            None => what.to_string(),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn event_phase_display_round_trip() {
        for phase in [EventPhase::Spawn, EventPhase::Despawn, EventPhase::Sweep] {
            let s = phase.to_string();
            assert_eq!(EventPhase::from_str(&s).unwrap(), phase, "round-trip {s}");
        }
    }

    #[test]
    fn event_outcome_display_round_trip() {
        for outcome in [EventOutcome::Failed, EventOutcome::Reaped] {
            let s = outcome.to_string();
            assert_eq!(
                EventOutcome::from_str(&s).unwrap(),
                outcome,
                "round-trip {s}"
            );
        }
    }

    #[test]
    fn unknown_phase_or_outcome_is_invalid() {
        assert!(matches!(
            EventPhase::from_str("resurrect"),
            Err(Error::Invalid)
        ));
        assert!(matches!(
            EventOutcome::from_str("maybe"),
            Err(Error::Invalid)
        ));
    }

    /// The whole reason both fields exist: the groupable key stays coarse, the
    /// operator-only detail keeps the stderr.
    #[test]
    fn a_failure_keeps_the_code_and_the_detail_apart() {
        let err = Error::Subprocess("ghost install: EACCES /opt/ghost".into());
        let event = InstanceEvent::failed("witty-warm-wombat", EventPhase::Spawn, &err);

        assert_eq!(event.code, "subprocess");
        assert_eq!(event.outcome, EventOutcome::Failed);
        assert_eq!(event.phase, EventPhase::Spawn);
        assert_eq!(
            event.detail.as_deref(),
            Some("subprocess error: ghost install: EACCES /opt/ghost")
        );
    }

    #[test]
    fn a_reap_carries_no_error() {
        let event = InstanceEvent::reaped("witty-warm-wombat", EventPhase::Sweep);

        assert_eq!(event.code, InstanceEvent::NO_ERROR);
        assert_eq!(event.outcome, EventOutcome::Reaped);
        assert_eq!(event.detail, None);
    }

    #[test]
    fn during_prefixes_the_detail_without_touching_the_code() {
        let event = InstanceEvent::failed("a-b-c", EventPhase::Despawn, &Error::NotFound)
            .during("releasing port 9000");

        assert_eq!(event.code, "not_found");
        assert_eq!(
            event.detail.as_deref(),
            Some("releasing port 9000: not found")
        );
    }
}
