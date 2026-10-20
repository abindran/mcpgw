//! The per-server lifecycle state machine (ADR-0004), as a pure function so
//! every transition is testable without processes.
//!
//! ```text
//!            Demand                StartOk            CallStart
//!   Cold ───────────▶ Starting ───────────▶ Ready ────────────▶ Busy{n}
//!    ▲  ▲                │ StartErr                ◀──────────  │  ▲
//!    │  │                ▼                          CallEnd(n=1)│  │CallStart
//!    │  └──RetryDue── Backoff{a} ◀──── Lost ─────── Idle ◀──────┘  │
//!    │                   │ (a = max)                   └───────────┘
//!    │                   ▼
//!    │                 Failed ──Reset──▶ Cold
//!    │
//!    └── Close ◀── Draining{n} ◀── Stop (from Ready / Idle / Busy / Starting)
//! ```
//!
//! `Disabled` servers never leave `Disabled`.

/// Where a server is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not running; starts on demand.
    Cold,
    /// Spawn and handshake in progress; calls queue.
    Starting,
    /// Connected and not yet used.
    Ready,
    /// Connected with `inflight` calls running.
    Busy {
        /// Calls in flight (≥ 1).
        inflight: u32,
    },
    /// Connected, no calls in flight.
    Idle,
    /// Stopping: no new calls start; waits for `inflight` to finish.
    Draining {
        /// Calls still running.
        inflight: u32,
    },
    /// Waiting before the next start after failure number `attempt`.
    Backoff {
        /// Consecutive failures so far (≥ 1).
        attempt: u32,
    },
    /// Gave up after too many consecutive failures; needs a reset.
    Failed,
    /// Not allowed to run (disabled, or transport not supported).
    Disabled,
}

impl State {
    /// Short name for status output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Busy { .. } => "busy",
            Self::Idle => "idle",
            Self::Draining { .. } => "draining",
            Self::Backoff { .. } => "backoff",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
        }
    }

    /// Whether a live connection exists.
    pub fn is_connected(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Busy { .. } | Self::Idle | Self::Draining { .. }
        )
    }
}

/// Something that happened to a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A caller needs the server running.
    Demand,
    /// The spawn and handshake succeeded.
    StartOk,
    /// The spawn or handshake failed.
    StartErr,
    /// A tool call started.
    CallStart,
    /// A tool call finished.
    CallEnd,
    /// The connection died or a health check failed.
    Lost,
    /// Stop the server (idle reaper, eviction, shutdown).
    Stop,
    /// The backoff delay elapsed.
    RetryDue,
    /// Operator reset of a failed server.
    Reset,
}

/// What the actor must do after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing.
    None,
    /// Spawn the server and run the handshake.
    Spawn,
    /// Close the connection (kills the process tree).
    Close,
    /// Close the connection (if any) and arm the backoff timer.
    Retry,
}

/// The event makes no sense in the state (a bug, or a stale message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    /// State at the time.
    pub state: State,
    /// The event.
    pub event: Event,
}

/// Compute the next state. `failures` is the number of consecutive failed
/// starts before this event; at `max_failures` a start failure is final.
pub fn step(
    state: &State,
    event: Event,
    failures: u32,
    max_failures: u32,
) -> Result<(State, Action), Invalid> {
    use Action as A;
    use Event as E;
    use State as S;
    let after_failure = || {
        let attempt = failures + 1;
        if attempt >= max_failures {
            (S::Failed, A::Close)
        } else {
            (S::Backoff { attempt }, A::Retry)
        }
    };
    let next = match (state, event) {
        (S::Disabled, _) => (S::Disabled, A::None),

        (S::Cold, E::Demand) => (S::Starting, A::Spawn),
        (S::Cold, E::Stop) => (S::Cold, A::None),

        (S::Starting, E::Demand) => (S::Starting, A::None),
        (S::Starting, E::StartOk) => (S::Ready, A::None),
        (S::Starting, E::StartErr) => after_failure(),
        // Can't abort a handshake; drain it and close when it lands.
        (S::Starting, E::Stop) => (S::Draining { inflight: 0 }, A::None),

        (S::Ready | S::Idle, E::Demand) => (state.clone(), A::None),
        (S::Ready | S::Idle, E::CallStart) => (S::Busy { inflight: 1 }, A::None),
        (S::Ready | S::Idle, E::Stop) => (S::Cold, A::Close),
        (S::Ready | S::Idle | S::Busy { .. }, E::Lost) => (S::Backoff { attempt: 1 }, A::Retry),

        (S::Busy { .. }, E::Demand) => (state.clone(), A::None),
        (S::Busy { inflight }, E::CallStart) => (
            S::Busy {
                inflight: inflight + 1,
            },
            A::None,
        ),
        (S::Busy { inflight: 1 }, E::CallEnd) => (S::Idle, A::None),
        (S::Busy { inflight }, E::CallEnd) => (
            S::Busy {
                inflight: inflight - 1,
            },
            A::None,
        ),
        (S::Busy { inflight }, E::Stop) => (
            S::Draining {
                inflight: *inflight,
            },
            A::None,
        ),

        (S::Draining { .. }, E::Demand | E::Stop) => (state.clone(), A::None),
        (S::Draining { inflight: 0 | 1 }, E::CallEnd) => (S::Cold, A::Close),
        (S::Draining { inflight }, E::CallEnd) => (
            S::Draining {
                inflight: inflight - 1,
            },
            A::None,
        ),
        (S::Draining { .. }, E::StartOk | E::Lost) => (S::Cold, A::Close),
        (S::Draining { .. }, E::StartErr) => (S::Cold, A::None),

        (S::Backoff { .. }, E::Demand) => (state.clone(), A::None),
        (S::Backoff { .. }, E::RetryDue) => (S::Cold, A::None),
        (S::Backoff { .. }, E::Stop) => (S::Cold, A::None),

        (S::Failed, E::Demand | E::Stop) => (S::Failed, A::None),
        (S::Failed, E::Reset) => (S::Cold, A::None),

        (state, event) => {
            return Err(Invalid {
                state: state.clone(),
                event,
            });
        }
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action as A;
    use Event as E;
    use State as S;

    const ALL_EVENTS: [Event; 9] = [
        E::Demand,
        E::StartOk,
        E::StartErr,
        E::CallStart,
        E::CallEnd,
        E::Lost,
        E::Stop,
        E::RetryDue,
        E::Reset,
    ];

    fn all_states() -> Vec<State> {
        vec![
            S::Cold,
            S::Starting,
            S::Ready,
            S::Busy { inflight: 1 },
            S::Busy { inflight: 3 },
            S::Idle,
            S::Draining { inflight: 0 },
            S::Draining { inflight: 1 },
            S::Draining { inflight: 2 },
            S::Backoff { attempt: 2 },
            S::Failed,
            S::Disabled,
        ]
    }

    /// The expected outcome of every (state, event) pair; `None` = invalid.
    fn expected(s: &State, e: Event) -> Option<(State, Action)> {
        Some(match (s, e) {
            (S::Disabled, _) => (S::Disabled, A::None),
            (S::Cold, E::Demand) => (S::Starting, A::Spawn),
            (S::Cold, E::Stop) => (S::Cold, A::None),
            (S::Starting, E::Demand) => (S::Starting, A::None),
            (S::Starting, E::StartOk) => (S::Ready, A::None),
            (S::Starting, E::StartErr) => (S::Backoff { attempt: 1 }, A::Retry),
            (S::Starting, E::Stop) => (S::Draining { inflight: 0 }, A::None),
            (S::Ready, E::Demand) => (S::Ready, A::None),
            (S::Idle, E::Demand) => (S::Idle, A::None),
            (S::Ready | S::Idle, E::CallStart) => (S::Busy { inflight: 1 }, A::None),
            (S::Ready | S::Idle, E::Stop) => (S::Cold, A::Close),
            (S::Ready | S::Idle | S::Busy { .. }, E::Lost) => (S::Backoff { attempt: 1 }, A::Retry),
            (S::Busy { .. }, E::Demand) => (s.clone(), A::None),
            (S::Busy { inflight: 1 }, E::CallStart) => (S::Busy { inflight: 2 }, A::None),
            (S::Busy { inflight: 3 }, E::CallStart) => (S::Busy { inflight: 4 }, A::None),
            (S::Busy { inflight: 1 }, E::CallEnd) => (S::Idle, A::None),
            (S::Busy { inflight: 3 }, E::CallEnd) => (S::Busy { inflight: 2 }, A::None),
            (S::Busy { inflight }, E::Stop) => (
                S::Draining {
                    inflight: *inflight,
                },
                A::None,
            ),
            (S::Draining { .. }, E::Demand | E::Stop) => (s.clone(), A::None),
            (S::Draining { inflight: 0 | 1 }, E::CallEnd) => (S::Cold, A::Close),
            (S::Draining { inflight: 2 }, E::CallEnd) => (S::Draining { inflight: 1 }, A::None),
            (S::Draining { .. }, E::StartOk | E::Lost) => (S::Cold, A::Close),
            (S::Draining { .. }, E::StartErr) => (S::Cold, A::None),
            (S::Backoff { .. }, E::Demand) => (s.clone(), A::None),
            (S::Backoff { .. }, E::RetryDue | E::Stop) => (S::Cold, A::None),
            (S::Failed, E::Demand | E::Stop) => (S::Failed, A::None),
            (S::Failed, E::Reset) => (S::Cold, A::None),
            _ => return None,
        })
    }

    #[test]
    fn every_state_event_pair() {
        let mut checked = 0;
        for s in all_states() {
            for e in ALL_EVENTS {
                let got = step(&s, e, 0, 5).ok();
                assert_eq!(got, expected(&s, e), "{s:?} + {e:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, 12 * 9);
    }

    #[test]
    fn invalid_pairs_are_reported_not_panicked() {
        let e = step(&S::Cold, E::CallEnd, 0, 5).unwrap_err();
        assert_eq!(
            e,
            Invalid {
                state: S::Cold,
                event: E::CallEnd
            }
        );
        assert!(step(&S::Idle, E::StartOk, 0, 5).is_err());
        assert!(step(&S::Backoff { attempt: 1 }, E::CallStart, 0, 5).is_err());
    }

    #[test]
    fn consecutive_start_failures_end_in_failed() {
        assert_eq!(
            step(&S::Starting, E::StartErr, 0, 3).unwrap(),
            (S::Backoff { attempt: 1 }, A::Retry)
        );
        assert_eq!(
            step(&S::Starting, E::StartErr, 1, 3).unwrap(),
            (S::Backoff { attempt: 2 }, A::Retry)
        );
        assert_eq!(
            step(&S::Starting, E::StartErr, 2, 3).unwrap(),
            (S::Failed, A::Close)
        );
        assert_eq!(
            step(&S::Failed, E::Reset, 3, 3).unwrap(),
            (S::Cold, A::None)
        );
    }

    #[test]
    fn a_full_session() {
        let mut s = S::Cold;
        for (e, want) in [
            (E::Demand, S::Starting),
            (E::Demand, S::Starting),
            (E::StartOk, S::Ready),
            (E::CallStart, S::Busy { inflight: 1 }),
            (E::CallStart, S::Busy { inflight: 2 }),
            (E::CallEnd, S::Busy { inflight: 1 }),
            (E::CallEnd, S::Idle),
            (E::CallStart, S::Busy { inflight: 1 }),
            (E::Stop, S::Draining { inflight: 1 }),
            (E::CallEnd, S::Cold),
        ] {
            s = step(&s, e, 0, 5).unwrap().0;
            assert_eq!(s, want, "after {e:?}");
        }
    }

    #[test]
    fn names_and_connected() {
        let connected: Vec<_> = all_states()
            .into_iter()
            .filter(State::is_connected)
            .map(|s| s.name())
            .collect();
        assert_eq!(
            connected,
            [
                "ready", "busy", "busy", "idle", "draining", "draining", "draining"
            ]
        );
    }
}
