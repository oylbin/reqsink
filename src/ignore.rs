/// Rules describing which requests should *not* be recorded in the request cache.
///
/// Ignoring a request only suppresses recording -- the response (including any
/// user-defined template for that route) is produced exactly as it would be
/// otherwise.
use serde_derive::{Serialize, Deserialize};
use std::fs::File;

use log::{info, error};

fn any_method() -> String {
    "*".to_string()
}

/// A single ignore rule.
///
/// `method` is matched case-insensitively and may be `*` (the default when the
/// key is omitted from the JSON file) to match any HTTP method. `path` is
/// matched against the request path with `*` acting as a wildcard for any
/// sequence of characters, including `/`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IgnoreRule {
    #[serde(default = "any_method")]
    pub method: String,
    pub path: String,
}

impl IgnoreRule {
    pub fn new(method: &str, path: &str) -> Self {
        IgnoreRule {
            method: method.to_string(),
            path: path.to_string(),
        }
    }

    pub fn matches(&self, method: &str, path: &str) -> bool {
        // Paths stay case-sensitive (they are, per RFC 3986); only the method is
        // normalised.
        (self.method == "*" || self.method.eq_ignore_ascii_case(method))
            && glob_match(&self.path, path)
    }
}

/// The rules applied unless `--no-default-ignore` is given. These are the
/// requests browsers send unprompted and that would otherwise flood the cache.
pub fn default_rules() -> Vec<IgnoreRule> {
    vec![
        IgnoreRule::new("GET", "/favicon.ico"),
        IgnoreRule::new("GET", "/"),
    ]
}

/// Build the active rule set from the built-in defaults plus an optional JSON
/// file. A malformed file is fatal on purpose: silently dropping rules would
/// leave the user believing they are in effect.
pub fn load_rules(file: Option<&String>, use_defaults: bool) -> Vec<IgnoreRule> {
    let mut rules = if use_defaults { default_rules() } else { Vec::new() };

    if let Some(path) = file {
        let f = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                error!("Could not open ignore-rules file {:?}: {}", path, e);
                std::process::exit(1);
            }
        };
        let parsed: Vec<IgnoreRule> = match serde_json::from_reader(&f) {
            Ok(r) => r,
            Err(e) => {
                error!("Could not parse ignore-rules file {:?}: {}", path, e);
                std::process::exit(1);
            }
        };
        rules.extend(parsed);
    }

    info!("{:?} request-ignore rule(s) active:", rules.len());
    for r in &rules {
        info!("  ignore {} {}", r.method, r.path);
    }
    rules
}

pub fn is_ignored(rules: &[IgnoreRule], method: &str, path: &str) -> bool {
    rules.iter().any(|r| r.matches(method, path))
}

/// Wildcard match supporting `*` only, with backtracking. Deliberately
/// hand-rolled rather than pulling in `regex`/`glob`: reqsink ships as a small
/// self-contained binary and `*` covers every case the rules need.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    let mut pi = 0;
    let mut ti = 0;
    // Position of the most recent `*` in the pattern, and how much of the text
    // it was assumed to consume, so we can backtrack and let it consume more.
    let mut star: Option<usize> = None;
    let mut star_ti = 0;

    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            star_ti += 1;
            ti = star_ti;
            pi = s + 1;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::{glob_match, is_ignored, default_rules, IgnoreRule};

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("/favicon.ico", "/favicon.ico"));
        assert!(!glob_match("/favicon.ico", "/favicon.icon"));
        assert!(!glob_match("/favicon.ico", "/Favicon.ico"));
        assert!(glob_match("/", "/"));
        assert!(!glob_match("/", "/api"));
    }

    #[test]
    fn glob_wildcards() {
        assert!(glob_match("/health*", "/health"));
        assert!(glob_match("/health*", "/healthz"));
        assert!(glob_match("/health*", "/health/live"));
        assert!(!glob_match("/health*", "/api/health"));

        assert!(glob_match("*", ""));
        assert!(glob_match("*", "/anything/at/all"));
        assert!(glob_match("*.ico", "/static/img/x.ico"));

        assert!(glob_match("/a/*/c", "/a/b/c"));
        assert!(glob_match("/a/*/c", "/a/b/b/c"));
        assert!(!glob_match("/a/*/c", "/a/b/d"));

        // Backtracking: the first `*` must give back characters for the tail to match
        assert!(glob_match("/*/widgets", "/api/v1/widgets"));
        assert!(!glob_match("/*/widgets", "/api/v1/widgets/1"));
    }

    #[test]
    fn method_matching_is_case_insensitive_and_supports_wildcard() {
        let get_only = IgnoreRule::new("GET", "/x");
        assert!(get_only.matches("GET", "/x"));
        assert!(get_only.matches("get", "/x"));
        assert!(!get_only.matches("POST", "/x"));

        let any = IgnoreRule::new("*", "/x");
        assert!(any.matches("GET", "/x"));
        assert!(any.matches("DELETE", "/x"));
    }

    #[test]
    fn method_defaults_to_wildcard_when_omitted() {
        let rules: Vec<IgnoreRule> =
            serde_json::from_str(r#"[{"path": "/health*"}]"#).unwrap();
        assert_eq!(rules[0].method, "*");
        assert!(rules[0].matches("POST", "/healthz"));
    }

    #[test]
    fn default_rules_cover_browser_noise_only() {
        let rules = default_rules();
        assert!(is_ignored(&rules, "GET", "/favicon.ico"));
        assert!(is_ignored(&rules, "GET", "/"));
        assert!(!is_ignored(&rules, "POST", "/"));
        assert!(!is_ignored(&rules, "GET", "/api/widgets"));
    }
}
