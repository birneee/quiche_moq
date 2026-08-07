/// Fragment parameter parser following HTTP query-parameter conventions.
///
/// Parameters appear after a `#` in the form `key[=value]` pairs separated by
/// `&`. A key with no `=` is a flag (boolean); a key with `=` carries a value.
/// Neither key nor value are URL-encoded — the caller must ensure they contain
/// no `#` or `&` characters.
///
/// Example: `tos--video#out=/tmp/out.mp4&priority=5&largest`
/// Example: `https://relay.example.com:4433#upstream`
///
/// Splitting is done by `FragmentParams::split`, which returns the head string
/// (before `#`) together with the parsed parameters.
pub struct FragmentParams<'a> {
    pairs: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> FragmentParams<'a> {
    /// Splits `s` on the first `#` and parses the right-hand side as
    /// `key[=value]` pairs separated by `&`. Returns `(head, params)`.
    pub fn split(s: &'a str) -> (&'a str, Self) {
        let (head, frag) = s.split_once('#').unwrap_or((s, ""));
        (head, Self::parse(frag))
    }

    /// Parses a bare fragment string (without the leading `#`).
    pub fn parse(frag: &'a str) -> Self {
        let pairs = frag
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (p, None),
            })
            .collect();
        Self { pairs }
    }

    /// Returns `true` if `key` is present (as a flag or with a value).
    pub fn has(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| *k == key)
    }

    /// Returns the value for `key`, if present and it has a value.
    /// Returns `None` if the key is absent or is a valueless flag.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.pairs.iter().find(|(k, _)| *k == key).and_then(|(_, v)| *v)
    }

    /// Returns the value for `key` as `Ok(value)`, or an error if the key
    /// is absent or present as a valueless flag.
    pub fn require(&self, key: &str) -> Result<&'a str, String> {
        match self.pairs.iter().find(|(k, _)| *k == key) {
            None => Err(format!("missing required parameter `{key}`")),
            Some((_, None)) => Err(format!("parameter `{key}` requires a value (`{key}=<value>`)")),
            Some((_, Some(v))) => Ok(v),
        }
    }

    /// Iterates over all `(key, value)` pairs. Value is `None` for flags.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.pairs.iter().map(|(k, v)| (*k, *v))
    }

    /// Returns an error listing any keys not in `known`, for use in
    /// catch-all unknown-parameter validation.
    pub fn reject_unknown(&self, known: &[&str]) -> Result<(), String> {
        let unknown: Vec<_> = self.pairs.iter()
            .filter(|(k, _)| !known.contains(k))
            .map(|(k, _)| *k)
            .collect();
        if unknown.is_empty() {
            Ok(())
        } else {
            let known_list = known.join(", ");
            Err(format!(
                "unknown parameter(s) {}: expected one of: {known_list}",
                unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
            ))
        }
    }
}

/// Asserts that a parameter parsed from `FragmentParams::iter` carries a value.
/// Use this inside a `match key { ... }` dispatch to reject bare-flag usage of
/// parameters that require a value (e.g. `#priority` with no `=N`).
///
/// Example (inside a match arm):
/// ```ignore
/// "priority" => priority = Some(require_value("priority", value)?.parse()...),
/// ```
/// Asserts that a flag parameter carries no value.
/// Use this for boolean parameters that must not have `=<value>`.
pub fn reject_value(key: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        None => Ok(()),
        Some(v) => Err(format!("parameter `{key}` is a flag and takes no value (got `{key}={v}`)")),
    }
}

/// Asserts that a parameter parsed from `FragmentParams::iter` carries a value.
/// Use this inside a `match key { ... }` dispatch to reject bare-flag usage of
/// parameters that require a value (e.g. `#priority` with no `=N`).
///
/// Example (inside a match arm):
/// ```ignore
/// "priority" => priority = Some(require_value("priority", value)?.parse()...),
/// ```
pub fn require_value<'a>(key: &str, value: Option<&'a str>) -> Result<&'a str, String> {
    value.ok_or_else(|| format!("parameter `{key}` requires a value (`{key}=<value>`)"))
}
