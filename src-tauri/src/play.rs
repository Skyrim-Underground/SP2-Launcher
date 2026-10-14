// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Play state machine: what one press of Play goes through, as pure transitions the launcher drives.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    #[default]
    Idle,
    VerifyingFiles,
    RequestingToken,
    Launching,
    Running,
    Reconnecting,
}

/// What happened; every failure carries the text the player sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Play,
    FilesVerified,
    FilesRefused(String),
    TokenGranted,
    TokenRefused(String),
    Launched,
    LaunchFailed(String),
    Reconnect,
    Reconnected,
    ReconnectFailed(String),
    GameExited {
        code: Option<i32>,
        /// Set when the game's files could not be put back; the next start puts them back.
        problem: Option<String>,
    },
}

/// What the launcher does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    VerifyFiles,
    RequestToken,
    Launch,
    RequestReconnectToken,
}

/// What the UI shows under the Play button once a step has ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Notice {
    Failed {
        message: String,
    },
    Reconnected,
    Exited {
        code: Option<i32>,
        problem: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct Machine {
    state: State,
    notice: Option<Notice>,
}

impl Machine {
    pub fn state(&self) -> State {
        self.state
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Applies the event and says what to do next; an event that does not belong to the state changes nothing.
    pub fn apply(&mut self, event: Event) -> Option<Effect> {
        let (state, effect, notice) = match (self.state, event) {
            (State::Idle, Event::Play) => (State::VerifyingFiles, Some(Effect::VerifyFiles), None),
            (State::VerifyingFiles, Event::FilesVerified) => {
                (State::RequestingToken, Some(Effect::RequestToken), None)
            }
            (State::VerifyingFiles, Event::FilesRefused(message)) => {
                (State::Idle, None, Some(Notice::Failed { message }))
            }
            (State::RequestingToken, Event::TokenGranted) => {
                (State::Launching, Some(Effect::Launch), None)
            }
            (State::RequestingToken, Event::TokenRefused(message)) => {
                (State::Idle, None, Some(Notice::Failed { message }))
            }
            (State::Launching, Event::Launched) => (State::Running, None, None),
            (State::Launching, Event::LaunchFailed(message)) => {
                (State::Idle, None, Some(Notice::Failed { message }))
            }
            (State::Running, Event::Reconnect) => (
                State::Reconnecting,
                Some(Effect::RequestReconnectToken),
                None,
            ),
            (State::Reconnecting, Event::Reconnected) => {
                (State::Running, None, Some(Notice::Reconnected))
            }
            (State::Reconnecting, Event::ReconnectFailed(message)) => {
                (State::Running, None, Some(Notice::Failed { message }))
            }
            (State::Running | State::Reconnecting, Event::GameExited { code, problem }) => {
                (State::Idle, None, Some(Notice::Exited { code, problem }))
            }
            _ => return None,
        };
        self.state = state;
        self.notice = notice;
        effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(message: &str) -> Option<Notice> {
        Some(Notice::Failed {
            message: message.to_string(),
        })
    }

    #[test]
    fn a_play_runs_through_verify_token_and_launch_to_running() {
        let mut machine = Machine::default();
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(machine.notice(), None);
        assert_eq!(machine.apply(Event::Play), Some(Effect::VerifyFiles));
        assert_eq!(machine.state(), State::VerifyingFiles);
        assert_eq!(
            machine.apply(Event::FilesVerified),
            Some(Effect::RequestToken)
        );
        assert_eq!(machine.state(), State::RequestingToken);
        assert_eq!(machine.apply(Event::TokenGranted), Some(Effect::Launch));
        assert_eq!(machine.state(), State::Launching);
        assert_eq!(machine.apply(Event::Launched), None);
        assert_eq!(machine.state(), State::Running);
        assert_eq!(machine.notice(), None);
        assert_eq!(
            machine.apply(Event::GameExited {
                code: Some(0),
                problem: None
            }),
            None
        );
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(
            machine.notice(),
            Some(&Notice::Exited {
                code: Some(0),
                problem: None
            })
        );
    }

    #[test]
    fn every_failure_returns_to_idle_with_its_message() {
        let mut machine = Machine::default();
        machine.apply(Event::Play);
        assert_eq!(
            machine.apply(Event::FilesRefused("files out of date".into())),
            None
        );
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(machine.notice(), failed("files out of date").as_ref());

        machine.apply(Event::Play);
        machine.apply(Event::FilesVerified);
        assert_eq!(machine.apply(Event::TokenRefused("banned".into())), None);
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(machine.notice(), failed("banned").as_ref());

        machine.apply(Event::Play);
        assert_eq!(machine.notice(), None, "a new Play clears the last notice");
        machine.apply(Event::FilesVerified);
        machine.apply(Event::TokenGranted);
        assert_eq!(machine.apply(Event::LaunchFailed("no loader".into())), None);
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(machine.notice(), failed("no loader").as_ref());
    }

    #[test]
    fn reconnect_asks_for_a_token_and_keeps_the_game_running_either_way() {
        let mut machine = Machine::default();
        machine.apply(Event::Play);
        machine.apply(Event::FilesVerified);
        machine.apply(Event::TokenGranted);
        machine.apply(Event::Launched);
        assert_eq!(
            machine.apply(Event::Reconnect),
            Some(Effect::RequestReconnectToken)
        );
        assert_eq!(machine.state(), State::Reconnecting);
        assert_eq!(machine.apply(Event::Reconnected), None);
        assert_eq!(machine.state(), State::Running);
        assert_eq!(machine.notice(), Some(&Notice::Reconnected));
        machine.apply(Event::Reconnect);
        assert_eq!(
            machine.apply(Event::ReconnectFailed("server offline".into())),
            None
        );
        assert_eq!(machine.state(), State::Running);
        assert_eq!(machine.notice(), failed("server offline").as_ref());
        // The game can end while a reconnect is in flight; the late answer then changes nothing.
        machine.apply(Event::Reconnect);
        machine.apply(Event::GameExited {
            code: None,
            problem: Some("plugins.txt is held open".into()),
        });
        assert_eq!(machine.state(), State::Idle);
        assert_eq!(machine.apply(Event::Reconnected), None);
        assert_eq!(machine.apply(Event::ReconnectFailed("late".into())), None);
        assert_eq!(
            machine.notice(),
            Some(&Notice::Exited {
                code: None,
                problem: Some("plugins.txt is held open".into())
            })
        );
    }

    #[test]
    fn events_outside_their_state_change_nothing() {
        let mut machine = Machine::default();
        for event in [
            Event::FilesVerified,
            Event::FilesRefused("x".into()),
            Event::TokenGranted,
            Event::TokenRefused("x".into()),
            Event::Launched,
            Event::LaunchFailed("x".into()),
            Event::Reconnect,
            Event::Reconnected,
            Event::ReconnectFailed("x".into()),
            Event::GameExited {
                code: None,
                problem: None,
            },
        ] {
            assert_eq!(machine.apply(event.clone()), None, "{event:?}");
            assert_eq!(machine.state(), State::Idle);
            assert_eq!(machine.notice(), None);
        }
        machine.apply(Event::Play);
        machine.apply(Event::FilesVerified);
        machine.apply(Event::TokenGranted);
        machine.apply(Event::Launched);
        assert_eq!(machine.apply(Event::Play), None, "one game at a time");
        assert_eq!(machine.apply(Event::FilesVerified), None);
        assert_eq!(machine.apply(Event::TokenGranted), None);
        assert_eq!(machine.state(), State::Running);
        machine.apply(Event::GameExited {
            code: Some(0),
            problem: None,
        });
        machine.apply(Event::Play);
        assert_eq!(
            machine.apply(Event::Reconnect),
            None,
            "nothing to reconnect yet"
        );
        assert_eq!(machine.state(), State::VerifyingFiles);
    }

    #[test]
    fn states_and_notices_serialize_for_the_ui() {
        assert_eq!(
            serde_json::to_string(&State::VerifyingFiles).unwrap(),
            "\"verifyingFiles\""
        );
        assert_eq!(
            serde_json::to_string(&Notice::Exited {
                code: Some(3),
                problem: None
            })
            .unwrap(),
            r#"{"kind":"exited","code":3,"problem":null}"#
        );
        assert_eq!(
            serde_json::to_string(&Notice::Failed {
                message: "x".into()
            })
            .unwrap(),
            r#"{"kind":"failed","message":"x"}"#
        );
    }
}
