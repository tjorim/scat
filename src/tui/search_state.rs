//! Terminal-independent search transitions. Existing rows/navigation belong to
//! the app and survive pending queries and errors. Supersession discards replies;
//! it does not interrupt a database operation already running on the worker.
use super::search_worker::SearchResponse;
use scat_core::core::db::JsonRow;
use std::time::Instant;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum Phase {
    #[default]
    Idle,
    Loading {
        id: u64,
    },
    Success,
    Error(String),
}

#[derive(Default)]
pub(super) struct SearchState {
    pub phase: Phase,
    pub pending_query: Option<String>,
    pub last_keystroke_at: Option<Instant>,
    next_id: u64,
}

impl SearchState {
    pub fn is_loading(&self) -> bool {
        matches!(self.phase, Phase::Loading { .. })
    }

    /// Editing immediately revokes the old request, including during debounce.
    pub fn schedule(&mut self, query: String, now: Instant) {
        self.phase = Phase::Idle;
        self.pending_query = Some(query);
        self.last_keystroke_at = Some(now);
    }

    /// Called before sending; transport errors must call `fail`.
    pub fn begin(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("search request ID exhausted");
        self.pending_query = None;
        self.last_keystroke_at = None;
        self.phase = Phase::Loading { id };
        id
    }

    pub fn fail(&mut self, error: String) {
        self.phase = Phase::Error(error);
    }

    /// Only the active request may transition state or replace displayed rows.
    pub fn accept(&mut self, response: SearchResponse) -> Option<Vec<JsonRow>> {
        if self.phase != (Phase::Loading { id: response.id }) {
            return None;
        }
        match response.result {
            Ok(rows) => {
                self.phase = Phase::Success;
                Some(rows)
            }
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(id: u64, result: Result<Vec<JsonRow>, &str>) -> SearchResponse {
        SearchResponse {
            id,
            result: result.map_err(str::to_owned),
        }
    }

    #[test]
    fn rapid_edits_revoke_replies_even_before_next_dispatch() {
        let mut state = SearchState::default();
        let first = state.begin();
        state.schedule("second".into(), Instant::now());
        assert!(!state.is_loading());
        assert!(state.accept(reply(first, Ok(vec![]))).is_none());
        assert_eq!(state.pending_query.as_deref(), Some("second"));
        let second = state.begin();
        assert!(state.accept(reply(first, Err("late error"))).is_none());
        assert_eq!(state.phase, Phase::Loading { id: second });
        assert_eq!(state.accept(reply(second, Ok(vec![]))), Some(vec![]));
        assert_eq!(state.phase, Phase::Success);
        assert!(state.accept(reply(second, Err("duplicate"))).is_none());
        assert_eq!(state.phase, Phase::Success);
    }

    #[test]
    fn errors_are_terminal_and_a_new_request_recovers() {
        let mut state = SearchState::default();
        let id = state.begin();
        assert!(state.accept(reply(id, Err("invalid query"))).is_none());
        assert_eq!(state.phase, Phase::Error("invalid query".into()));
        let next = state.begin();
        assert_ne!(id, next);
        assert_eq!(state.accept(reply(next, Ok(vec![]))), Some(vec![]));
        state.begin();
        state.fail("disconnected".into());
        assert!(!state.is_loading());
    }
}
