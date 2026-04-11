use super::config::Route;

/// Find the best-matching route for a hostname.
///
/// Priority rules:
///   1. Exact match wins unconditionally ("api.foo.test" beats "*.foo.test")
///   2. Among wildcards, the most specific (longest pattern) wins
///      so "*.sub.foo.test" beats "*.foo.test" for "x.sub.foo.test"
pub fn match_route<'a>(host: &str, routes: &'a [Route]) -> Option<&'a Route> {
    // 1. Exact match
    if let Some(r) = routes
        .iter()
        .find(|r| !r.domain.contains('*') && r.domain == host)
    {
        return Some(r);
    }

    // 2. Wildcard — "*.foo.test" matches "bar.foo.test" (suffix = ".foo.test")
    routes
        .iter()
        .filter(|r| r.domain.starts_with("*."))
        .filter(|r| {
            let suffix = &r.domain[1..]; // "*.foo.test" → ".foo.test"
            host.ends_with(suffix)
        })
        .max_by_key(|r| r.domain.len()) // longer pattern = more specific
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(domain: &str, upstream: &str) -> Route {
        Route {
            domain: domain.to_string(),
            upstream: upstream.to_string(),
        }
    }

    #[test]
    fn exact_beats_wildcard() {
        let routes = [
            r("*.newline.test", "wildcard:8080"),
            r("backend.newline.test", "backend:3000"),
        ];
        assert_eq!(
            match_route("backend.newline.test", &routes)
                .unwrap()
                .upstream,
            "backend:3000"
        );
    }

    #[test]
    fn wildcard_catches_other_subdomain() {
        let routes = [
            r("*.newline.test", "wildcard:8080"),
            r("backend.newline.test", "backend:3000"),
        ];
        assert_eq!(
            match_route("api.newline.test", &routes).unwrap().upstream,
            "wildcard:8080"
        );
    }

    #[test]
    fn most_specific_wildcard_wins() {
        let routes = [
            r("*.test", "generic:8080"),
            r("*.newline.test", "newline:9090"),
        ];
        assert_eq!(
            match_route("api.newline.test", &routes).unwrap().upstream,
            "newline:9090"
        );
    }

    #[test]
    fn no_match_returns_none() {
        let routes = [r("*.newline.test", "wildcard:8080")];
        assert!(match_route("other.test", &routes).is_none());
    }

    #[test]
    fn wildcard_does_not_match_base_domain() {
        // "*.foo.test" should NOT match "foo.test" itself — suffix ".foo.test" needs a dot prefix
        let routes = [r("*.foo.test", "wildcard:8080")];
        assert!(match_route("foo.test", &routes).is_none());
    }
}
