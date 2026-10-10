//! The request model (`Slim::Control::Request`).

use indexmap::IndexMap;
use serde_json::{Map, Value};
use slimproto::MacAddress;

pub type Results = Map<String, Value>;

/// Request status codes, numerically identical to the reference server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    New = 0,
    Dispatchable = 1,
    Processing = 3,
    Done = 10,
    BadDispatch = 101,
    BadParams = 102,
    NeedsClient = 103,
    NotDispatchable = 104,
    BadConfig = 105,
}

impl Status {
    pub fn is_error(self) -> bool {
        (self as u16) > 100
    }
}

/// Where a request came from; used to avoid echoing a CLI connection's own commands back to it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Internal,
    Cli(u64),
    JsonRpc,
    Cometd(String),
    Web,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub client: Option<MacAddress>,
    /// Client id given for a `needsClient = 2` command when the client is not connected.
    pub disconnected_client: Option<String>,
    /// Matched command words (`["playlist", "index"]`).
    pub terms: Vec<String>,
    /// Positional (by dispatch name or `_pN`) and tagged params, in insertion order.
    pub params: IndexMap<String, Option<String>>,
    pub results: Results,
    pub status: Status,
    pub is_query: bool,
    pub source: Source,
    /// Index into the dispatch table, when matched.
    pub(crate) entry: Option<usize>,
    /// Language override (`Accept-Language`), upper-case code like `EN`.
    pub language: Option<String>,
}

impl Request {
    pub fn blank() -> Request {
        Request {
            client: None,
            disconnected_client: None,
            terms: Vec::new(),
            params: IndexMap::new(),
            results: Results::new(),
            status: Status::NotDispatchable,
            is_query: false,
            source: Source::Internal,
            entry: None,
            language: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.status == Status::Done
    }

    pub fn set_done(&mut self) {
        self.status = Status::Done;
    }

    pub fn bad_params(&mut self) {
        self.status = Status::BadParams;
    }

    pub fn bad_dispatch(&mut self) {
        self.status = Status::BadDispatch;
    }

    pub fn term(&self, i: usize) -> &str {
        self.terms.get(i).map(String::as_str).unwrap_or("")
    }

    /// `"playlist,index"`, used for notification filtering.
    pub fn request_str(&self) -> String {
        self.terms.join(",")
    }

    pub fn is_command(&self, words: &[&[&str]]) -> bool {
        words.iter().enumerate().all(|(i, alts)| alts.contains(&self.term(i)))
    }

    // ---- params -----------------------------------------------------------------

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).and_then(|v| v.as_deref())
    }

    /// Param that is defined and non-empty.
    pub fn param_nonempty(&self, name: &str) -> Option<&str> {
        self.param(name).filter(|s| !s.is_empty())
    }

    pub fn param_i64(&self, name: &str) -> Option<i64> {
        self.param(name).and_then(|s| {
            let s = s.trim();
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        })
    }

    pub fn param_f64(&self, name: &str) -> Option<f64> {
        self.param(name).and_then(|s| s.trim().parse::<f64>().ok())
    }

    pub fn param_bool(&self, name: &str) -> bool {
        matches!(self.param(name), Some(s) if !s.is_empty() && s != "0")
    }

    pub fn add_param(&mut self, name: &str, value: impl Into<String>) {
        self.params.insert(name.to_string(), Some(value.into()));
    }

    pub fn delete_param(&mut self, name: &str) {
        self.params.shift_remove(name);
    }

    /// All tagged (non-positional) params.
    pub fn tagged_params(&self) -> IndexMap<String, String> {
        self.params
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
            .collect()
    }

    // ---- results ----------------------------------------------------------------

    pub fn add_result(&mut self, key: &str, value: impl Into<Value>) {
        self.results.insert(key.to_string(), value.into());
    }

    pub fn set_result_first(&mut self, key: &str, value: impl Into<Value>) {
        let mut m = Results::new();
        m.insert(key.to_string(), value.into());
        for (k, v) in std::mem::take(&mut self.results) {
            if k != key {
                m.insert(k, v);
            }
        }
        self.results = m;
    }

    pub fn result(&self, key: &str) -> Option<&Value> {
        self.results.get(key)
    }

    /// Append an element to `<loop>` (which must end in `_loop`).
    pub fn add_loop_item(&mut self, loop_name: &str, item: Results) {
        match self.results.entry(loop_name.to_string()).or_insert_with(|| Value::Array(Vec::new())) {
            Value::Array(a) => a.push(Value::Object(item)),
            other => *other = Value::Array(vec![Value::Object(item)]),
        }
    }

    pub fn loop_len(&self, loop_name: &str) -> usize {
        match self.results.get(loop_name) {
            Some(Value::Array(a)) => a.len(),
            _ => 0,
        }
    }

    /// Merge a whole result map (e.g. from the library) into this request's results.
    pub fn merge_results(&mut self, other: Results) {
        for (k, v) in other {
            self.results.insert(k, v);
        }
    }

    /// Render like `renderAsArray`: terms, params (`_x` bare, `x:v`), results (loops flattened).
    pub fn render_as_array(&self) -> Vec<String> {
        let mut out: Vec<String> = self.terms.clone();
        for (k, v) in &self.params {
            if k.starts_with("__") {
                continue;
            }
            let v = v.clone().unwrap_or_default();
            if k.starts_with('_') {
                out.push(v);
            } else {
                out.push(format!("{}:{}", k, v));
            }
        }
        fn scalar(v: &Value) -> String {
            match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                Value::Array(a) => a.iter().map(scalar).collect::<Vec<_>>().join(","),
                Value::Object(_) => "HASH".to_string(),
                other => other.to_string(),
            }
        }
        for (k, v) in &self.results {
            if k.starts_with("__") {
                continue;
            }
            if k.ends_with("_loop") {
                if let Value::Array(items) = v {
                    for item in items {
                        if let Value::Object(m) = item {
                            for (k2, v2) in m {
                                if k2.starts_with("__") {
                                    continue;
                                }
                                if k2.starts_with('_') {
                                    out.push(scalar(v2));
                                } else {
                                    out.push(format!("{}:{}", k2, scalar(v2)));
                                }
                            }
                        }
                    }
                }
                continue;
            }
            if k.starts_with('_') {
                out.push(scalar(v));
            } else {
                out.push(format!("{}:{}", k, scalar(v)));
            }
        }
        out
    }
}

/// `Request::normalize`: returns (valid, start, end) for paging over `count` items.
pub fn normalize(from: Option<i64>, quantity: Option<i64>, count: usize) -> (bool, usize, usize) {
    let count = count as i64;
    let mut quantity = quantity;
    if quantity.is_none() && from.is_some() {
        quantity = Some(count);
    }
    let from = from.unwrap_or(0);
    let quantity = quantity.unwrap_or(0);
    if quantity == 0 || count == 0 || from > count - 1 {
        return (false, 0, 0);
    }
    let start = from.max(0);
    let end = (start + quantity - 1).min(count - 1);
    (true, start as usize, end as usize)
}

/// Positional `<start>` / `<itemsPerResponse>` params as LMS treats them (`-` = current).
pub fn paging(req: &Request) -> (Option<i64>, Option<i64>) {
    (req.param_i64("_index"), req.param_i64("_quantity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rules() {
        assert_eq!(normalize(Some(0), Some(10), 3), (true, 0, 2));
        assert_eq!(normalize(Some(5), Some(10), 3), (false, 0, 0));
        assert_eq!(normalize(Some(0), Some(0), 3), (false, 0, 0));
        assert_eq!(normalize(Some(1), None, 3), (true, 1, 2));
        assert_eq!(normalize(Some(-2), Some(2), 3), (true, 0, 1));
    }

    #[test]
    fn render() {
        let mut r = Request::blank();
        r.terms = vec!["players".into()];
        r.params.insert("_index".into(), Some("0".into()));
        r.params.insert("_quantity".into(), Some("2".into()));
        r.params.insert("tags".into(), Some("a".into()));
        r.add_result("count", 1);
        let mut item = Results::new();
        item.insert("playerindex".into(), Value::from(0));
        item.insert("name".into(), Value::from("Kitchen"));
        r.add_loop_item("players_loop", item);
        r.add_result("__hidden", 1);
        r.add_result("_bare", "x");
        assert_eq!(r.render_as_array(), vec!["players", "0", "2", "tags:a", "count:1", "playerindex:0", "name:Kitchen", "x"]);
    }
}
