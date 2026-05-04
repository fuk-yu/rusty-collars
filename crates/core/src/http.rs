//! HTTP content-negotiation helpers shared between firmware and host tests.

/// Returns `true` when the client's `Accept-Encoding` header signals that
/// `gzip` is an acceptable response coding per RFC 9110 §12.5.3.
///
/// `header_value` is the raw header field value as received from the client,
/// or `None` when the header is absent. Per the firmware's expected behavior
/// (see HTTP server bug report), an absent header is treated as "identity
/// only" rather than "any coding acceptable" — non-browser clients (curl,
/// monitoring probes, embedded clients) routinely omit `Accept-Encoding` and
/// cannot decode gzip blobs.
pub fn accepts_gzip(header_value: Option<&str>) -> bool {
    let Some(s) = header_value else {
        return false;
    };

    let mut wildcard_acceptable = false;
    let mut gzip_explicit: Option<bool> = None;

    for entry in s.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let mut parts = entry.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let mut q = 1.0_f32;
        for param in parts {
            if let Some(value) = parse_q_param(param.trim()) {
                q = value;
            }
        }

        if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") {
            gzip_explicit = Some(q > 0.0);
        } else if coding == "*" && gzip_explicit.is_none() {
            wildcard_acceptable = q > 0.0;
        }
    }

    gzip_explicit.unwrap_or(wildcard_acceptable)
}

fn parse_q_param(param: &str) -> Option<f32> {
    let (key, rest) = param.split_once('=')?;
    if !key.trim().eq_ignore_ascii_case("q") {
        return None;
    }
    rest.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::accepts_gzip;

    #[test]
    fn absent_header_means_identity_only() {
        assert!(!accepts_gzip(None));
    }

    #[test]
    fn identity_only_rejects_gzip() {
        assert!(!accepts_gzip(Some("identity")));
        assert!(!accepts_gzip(Some("identity;q=1")));
        assert!(!accepts_gzip(Some("identity;q=1, *;q=0")));
    }

    #[test]
    fn explicit_gzip_accepts() {
        assert!(accepts_gzip(Some("gzip")));
        assert!(accepts_gzip(Some("gzip, deflate")));
        assert!(accepts_gzip(Some("deflate, gzip")));
        assert!(accepts_gzip(Some("gzip;q=0.5")));
        assert!(accepts_gzip(Some("gzip;q=1.0")));
    }

    #[test]
    fn explicit_gzip_zero_rejects() {
        assert!(!accepts_gzip(Some("gzip;q=0")));
        assert!(!accepts_gzip(Some("gzip; q=0")));
        assert!(!accepts_gzip(Some("gzip;q=0.0")));
    }

    #[test]
    fn wildcard_accepts_when_gzip_unmentioned() {
        assert!(accepts_gzip(Some("*")));
        assert!(accepts_gzip(Some("deflate, *;q=0.5")));
    }

    #[test]
    fn wildcard_zero_rejects_unmentioned_gzip() {
        assert!(!accepts_gzip(Some("*;q=0")));
        assert!(!accepts_gzip(Some("identity, *;q=0")));
    }

    #[test]
    fn explicit_gzip_overrides_wildcard() {
        // gzip;q=0 with *;q=1 → gzip is forbidden, wildcard does not rescue it.
        assert!(!accepts_gzip(Some("gzip;q=0, *")));
        // gzip;q=1 with *;q=0 → gzip is allowed.
        assert!(accepts_gzip(Some("gzip, *;q=0")));
    }

    #[test]
    fn case_insensitive_and_x_gzip_alias() {
        assert!(accepts_gzip(Some("GZIP")));
        assert!(accepts_gzip(Some("Gzip")));
        assert!(accepts_gzip(Some("x-gzip")));
        assert!(accepts_gzip(Some("X-GZIP;Q=0.5")));
        assert!(!accepts_gzip(Some("X-GZIP;Q=0")));
    }

    #[test]
    fn realistic_browser_header() {
        assert!(accepts_gzip(Some("gzip, deflate, br, zstd")));
    }

    #[test]
    fn malformed_q_param_falls_back_to_default() {
        // An unparseable q-parameter is ignored, leaving the default q=1.0 in
        // effect — gzip is still accepted. Non-malicious clients don't send
        // unparseable q-values; we choose the lenient interpretation.
        assert!(accepts_gzip(Some("gzip;q=garbage")));
    }
}
