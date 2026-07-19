use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::db::JsonRow;
use scat_core::core::search::{SearchApi, open_search_api};

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub id: u64,
    pub query: String,
    pub limit: usize,
}

#[derive(Debug)]
pub struct SearchResponse {
    pub id: u64,
    pub result: std::result::Result<Vec<JsonRow>, String>,
}

pub struct SearchWorker {
    request_tx: Option<Sender<SearchRequest>>,
    response_rx: Receiver<SearchResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl SearchWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<SearchRequest>();
        let (response_tx, response_rx) = mpsc::channel::<SearchResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-search-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI search worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: SearchRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("search worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("search worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<SearchResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("search worker channel disconnected")),
        }
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, signaling the worker loop to exit.
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    db_path: PathBuf,
    request_rx: Receiver<SearchRequest>,
    response_tx: Sender<SearchResponse>,
) {
    let api = open_search_api(&db_path).map_err(|err| err.to_string());
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let result = match &api {
            Ok(api) => {
                run_search(api, &request.query, request.limit).map_err(|err| err.to_string())
            }
            Err(err) => Err(err.clone()),
        };
        if response_tx
            .send(SearchResponse {
                id: request.id,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

/// A TUI query split into free text and `lang:`/`owner:`/`tag:` filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub text: String,
    pub lang: Option<String>,
    pub owner: Option<String>,
    pub tag: Option<String>,
}

impl ParsedQuery {
    /// Active filters as `key=value` pairs, for display.
    pub fn filter_labels(&self) -> Vec<String> {
        [
            ("lang", &self.lang),
            ("owner", &self.owner),
            ("tag", &self.tag),
        ]
        .iter()
        .filter_map(|(key, value)| value.as_deref().map(|value| format!("{key}={value}")))
        .collect()
    }
}

/// Split a search query into free text and filter tokens.
///
/// A whitespace-separated token of the form `lang:python` (also `language:`),
/// `owner:alice`, or `tag:deploy` becomes a filter matching the CLI's
/// `--lang`/`--owner`/`--tag` flags; the last occurrence of a key wins. A
/// filter key with an empty value (`lang:` mid-typing) is ignored. Everything
/// else stays part of the text query.
pub fn parse_query_filters(query: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery::default();
    let mut text_tokens: Vec<&str> = Vec::new();
    for token in query.split_whitespace() {
        let target = match token.split_once(':') {
            Some(("lang" | "language", value)) => Some((&mut parsed.lang, value)),
            Some(("owner", value)) => Some((&mut parsed.owner, value)),
            Some(("tag", value)) => Some((&mut parsed.tag, value)),
            _ => None,
        };
        match target {
            Some((slot, value)) if !value.is_empty() => *slot = Some(value.to_string()),
            Some(_) => {}
            None => text_tokens.push(token),
        }
    }
    parsed.text = text_tokens.join(" ");
    parsed
}

fn run_search(api: &SearchApi, query: &str, limit: usize) -> Result<Vec<JsonRow>> {
    let parsed = parse_query_filters(query);
    let (lang, owner, tag) = (
        parsed.lang.as_deref(),
        parsed.owner.as_deref(),
        parsed.tag.as_deref(),
    );
    let text = parsed.text.trim();
    if text.is_empty() {
        return api
            .list_scripts(lang, owner, tag, limit, 0)
            .context("failed to list scripts");
    }
    if crate::commands::query_uses_fts(text) {
        let live = auto_prefix_last_term(text);
        api.search_with_filters(&live, limit, lang, owner, tag)
            .context("failed to run search query")
    } else {
        // The INSTR path search matches `/`-separated logical paths, so
        // normalise Windows separators from the query first.
        let path_query = text.replace('\\', "/");
        api.search_by_path_with_filters(&path_query, limit, lang, owner, tag)
            .context("failed to run path search query")
    }
}

/// Append `*` to the last (still-being-typed) term so live search matches
/// as a prefix instead of requiring the complete word.
///
/// FTS5 has no implicit prefix matching, so a search-as-you-type box would
/// otherwise only ever match once the current word is typed out in full.
/// The last term is left untouched if it already ends in `*`, or if the
/// user explicitly closed it in double quotes — quoting is the existing
/// escape hatch for "match this exactly," and extending it with a `*`
/// would silently break that guarantee.
fn auto_prefix_last_term(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() || trimmed.ends_with('*') {
        return text.to_string();
    }
    // An odd number of quotes means the last term is an unterminated quoted
    // phrase (still being typed); an even count ending in `"` means it was
    // explicitly closed. Either way, leave quoting semantics alone.
    if trimmed.matches('"').count() % 2 == 1 || trimmed.ends_with('"') {
        return text.to_string();
    }
    format!("{trimmed}*")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{SearchRequest, SearchWorker};
    use scat_core::core::db::{SCHEMA_VERSION, create_db};
    use tempfile::NamedTempFile;

    fn recv_response(worker: &SearchWorker) -> super::SearchResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for search worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn make_db() -> NamedTempFile {
        let db = NamedTempFile::new().unwrap();
        let conn = create_db(db.path()).unwrap();
        conn.execute(
            "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?)",
            rusqlite::params![SCHEMA_VERSION],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/a.py','python','needle alpha','alice','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/b.py','python','beta only','bob','')",
            [],
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn empty_query_returns_all_scripts() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 1,
                query: String::new(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        let rows = response.result.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn parse_query_filters_splits_filters_and_text() {
        let parsed = super::parse_query_filters("backup lang:python owner:alice tag:deploy job");
        assert_eq!(parsed.text, "backup job");
        assert_eq!(parsed.lang.as_deref(), Some("python"));
        assert_eq!(parsed.owner.as_deref(), Some("alice"));
        assert_eq!(parsed.tag.as_deref(), Some("deploy"));
        assert_eq!(
            parsed.filter_labels(),
            vec!["lang=python", "owner=alice", "tag=deploy"]
        );
    }

    #[test]
    fn parse_query_filters_last_key_wins_and_empty_value_ignored() {
        let parsed = super::parse_query_filters("lang:shell language:python owner:");
        assert_eq!(parsed.lang.as_deref(), Some("python"));
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.text, "");
    }

    #[test]
    fn parse_query_filters_keeps_unknown_and_path_tokens_as_text() {
        let parsed = super::parse_query_filters("size:big /catalog/scripts/foo.py");
        assert_eq!(parsed.text, "size:big /catalog/scripts/foo.py");
        assert_eq!(
            parsed,
            super::ParsedQuery {
                text: parsed.text.clone(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn filter_only_query_lists_matching_scripts() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 2,
                query: "owner:alice".to_string(),
                limit: 200,
            })
            .unwrap();
        let rows = recv_response(&worker).result.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("logical_path").unwrap().as_str().unwrap(),
            "/catalog/scripts/a.py"
        );
    }

    #[test]
    fn text_query_with_filter_applies_both() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 3,
                query: "owner:alice beta".to_string(),
                limit: 200,
            })
            .unwrap();
        let rows = recv_response(&worker).result.unwrap();
        assert!(rows.is_empty(), "beta matches b.py, but bob owns it");
    }

    #[test]
    fn path_query_routes_to_path_search_instead_of_fts() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 4,
                query: "scripts/a".to_string(),
                limit: 200,
            })
            .unwrap();
        let rows = recv_response(&worker).result.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("logical_path").unwrap().as_str().unwrap(),
            "/catalog/scripts/a.py"
        );
    }

    #[test]
    fn auto_prefix_last_term_appends_star_to_bare_trailing_word() {
        assert_eq!(super::auto_prefix_last_term("consol"), "consol*");
        assert_eq!(super::auto_prefix_last_term("foo bar"), "foo bar*");
    }

    #[test]
    fn auto_prefix_last_term_leaves_explicit_prefix_and_quotes_alone() {
        assert_eq!(super::auto_prefix_last_term("consol*"), "consol*");
        assert_eq!(super::auto_prefix_last_term("\"consol\""), "\"consol\"");
        assert_eq!(super::auto_prefix_last_term("foo \"bar\""), "foo \"bar\"");
        // Still-open quote (mid-typing a phrase): leave as-is too.
        assert_eq!(super::auto_prefix_last_term("foo \"bar"), "foo \"bar");
    }

    #[test]
    fn auto_prefix_last_term_handles_empty_and_whitespace() {
        assert_eq!(super::auto_prefix_last_term(""), "");
        assert_eq!(super::auto_prefix_last_term("foo "), "foo*");
    }

    #[test]
    fn partial_word_now_matches_via_live_auto_prefix() {
        // "alph" alone would find nothing under plain FTS5 token matching;
        // the search worker auto-prefixes the trailing term so live typing
        // finds /catalog/scripts/a.py (content "needle alpha") before the
        // word is finished.
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 9,
                query: "alph".to_string(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 9);
        let rows = response.result.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("logical_path").unwrap().as_str().unwrap(),
            "/catalog/scripts/a.py"
        );
    }

    #[test]
    fn explicitly_quoted_partial_term_is_not_auto_prefixed() {
        // Quoting is the escape hatch for "match exactly"; a quoted partial
        // word must not match, unlike the bare equivalent above.
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 10,
                query: "\"alph\"".to_string(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 10);
        assert_eq!(response.result.unwrap().len(), 0);
    }

    #[test]
    fn previously_invalid_fts_syntax_no_longer_errors() {
        // These used to reach FTS5's MATCH raw and error (unbalanced quote,
        // hyphen-as-column-exclusion-operator); the query is now sanitized
        // before it reaches FTS5, so both come back Ok — a lone `"` finds
        // nothing meaningful to search for, while the hyphenated query
        // matches literally instead of erroring out.
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();

        worker
            .send(SearchRequest {
                id: 7,
                query: "\"".to_string(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 7);
        assert_eq!(response.result.unwrap().len(), 0);

        worker
            .send(SearchRequest {
                id: 8,
                query: "needle-alpha".to_string(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 8);
        let rows = response.result.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("logical_path").unwrap().as_str().unwrap(),
            "/catalog/scripts/a.py"
        );
    }
}
