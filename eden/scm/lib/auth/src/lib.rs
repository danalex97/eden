/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{collections::HashMap, convert::TryFrom, path::PathBuf, str};

use anyhow::{Error, Result};
use indexmap::IndexMap;
use url::Url;

use configparser::{config::ConfigSet, Text};
use util::path::expand_path;

pub mod x509;

pub use x509::{check_certs, X509Error};

/// A group of client authentiation settings from the user's config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Auth {
    pub group: String,
    pub prefix: String,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub cacerts: Option<PathBuf>,
    pub username: Option<String>,
    pub schemes: Vec<String>,
    pub priority: i32,
    pub extras: HashMap<String, String>,
}

impl TryFrom<(&str, HashMap<&str, Text>)> for Auth {
    type Error = Error;

    fn try_from((group, mut settings): (&str, HashMap<&str, Text>)) -> Result<Self> {
        let group = group.into();

        let prefix = settings
            .remove("prefix")
            .map(|s| s.to_string())
            .ok_or_else(|| Error::msg("auth prefix missing"))?;

        let cert = settings
            .remove("cert")
            .filter(|s| !s.is_empty())
            .map(expand_path);
        let key = settings
            .remove("key")
            .filter(|s| !s.is_empty())
            .map(expand_path);
        let cacerts = settings
            .remove("cacerts")
            .filter(|s| !s.is_empty())
            .map(expand_path);

        let username = settings.remove("username").map(|s| s.to_string());

        let schemes = settings.remove("schemes").map_or_else(
            || vec!["https".into()],
            |line| line.split(' ').map(String::from).collect(),
        );

        let priority = settings
            .remove("priority")
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or_default();

        let extras = settings
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>();

        Ok(Self {
            group,
            prefix,
            cert,
            key,
            cacerts,
            username,
            schemes,
            priority,
            extras,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    groups: Vec<Auth>,
    validate: bool,
}

impl AuthConfig {
    /// Parse the `[auth]` section of a Mercurial config into a map
    /// of grouped auth settings.
    ///
    /// The keys of the resulting HashMap are group names from the config;
    /// i.e., the first component of a key of the form `group.setting`.
    /// All keys in the auth section are expected to follow this format.
    ///
    /// Values are parsed `Auth` structs containing all of the values
    /// found for the given grouping.
    pub fn new(config: &ConfigSet) -> Self {
        // Use an IndexMap to preserve ordering; needed to correctly handle precedence.
        let mut groups = IndexMap::new();

        let keys = config.keys("auth");
        for key in &keys {
            // Skip keys that aren't valid UTF-8 or that don't match
            // the expected auth key format of `group.setting`.
            let (group, setting) = match key.find('.') {
                Some(i) => (&key[..i], &key[i + 1..]),
                None => continue,
            };
            if let Some(value) = config.get("auth", key) {
                groups
                    .entry(group)
                    .or_insert_with(HashMap::new)
                    .insert(setting, value);
            }
        }

        let groups = groups
            .into_iter()
            .filter_map(|group| Auth::try_from(group).ok())
            .collect();

        Self {
            groups,
            validate: false,
        }
    }

    /// Specify whether certificates should be checked for validity.
    ///
    /// If enabled, each candidate auth group will be checked to ensure that the
    /// certificate is present and valid, resulting in an error if the best
    /// candidate certificate is missing or invalid.
    ///
    /// If disabled (the default), groups with missing or invalid certificates
    /// will simply be ignored, which means that in the case that all candidate
    /// auth groups are invalid, `auth_for_url` will report that there are no
    /// matching auth groups instead of returning an error.
    ///
    /// This option exists to ensure backwards-compatibility with the old
    /// behavior in the case of missing certificates, which was to ignore them.
    /// The behavior will still be different in the case of invalid certificates
    /// because the old behavior did not check validity at all, meaning that
    /// invalid certificates will now be ignored if validation is disabled.
    ///
    /// TODO(kulshrax): This is a hack intended to address the fact that our
    /// configs in some environments do specify missing certificates (which
    /// were previously ignored). This option should be removed entirely once
    /// those configs have been fixed.
    pub fn validate(&mut self, enable: bool) -> &Self {
        self.validate = enable;
        self
    }

    /// Find the best matching auth group for the given URL.
    pub fn auth_for_url(&self, url: &Url) -> Result<Option<Auth>, X509Error> {
        let mut best: Option<&Auth> = None;
        let mut error: Option<X509Error> = None;

        let scheme = url.scheme().to_string();
        let username = url.username();
        let url_suffix = strip_scheme_and_user(&url);

        for auth in &self.groups {
            if !auth.schemes.contains(&scheme) {
                continue;
            }

            // If the URL contains a username, the entry's username must
            // match if the entry's username field is non-None.
            if !username.is_empty() {
                match auth.username {
                    Some(ref u) if u != username => continue,
                    _ => {}
                }
            }

            if auth.prefix != "*" && !url_suffix.starts_with(&auth.prefix) {
                continue;
            }

            // If there is an existing candidate, check whether the current
            // auth entry is a more specific match.
            if let Some(ref best) = best {
                // Take the entry with the longer prefix.
                if auth.prefix.len() < best.prefix.len() {
                    continue;
                } else if auth.prefix.len() == best.prefix.len() {
                    // If prefixes are the same, break the tie using priority.
                    if auth.priority < best.priority {
                        continue;
                    // If the priorities are the same, prefer entries with usernames.
                    } else if auth.priority == best.priority
                        && best.username.is_some()
                        && auth.username.is_none()
                    {
                        continue;
                    }
                }
            }

            // Skip if the cert is missing or invalid since there may be other
            // matching auth groups with valid certs, but hang on to the error
            // in case there aren't any others.
            if let Some(path) = &auth.cert {
                if let Err(e) = check_certs(path) {
                    // If validatation is disabled, just pretend this auth group
                    // doesn't exist instead of reporting the error.
                    if self.validate {
                        error = Some(e);
                    }
                    continue;
                }
            }

            best = Some(auth);
        }

        // If we encountered a missing or invalid cert, only return an error
        // if there were no other matching valid certs.
        match (best, error) {
            (Some(auth), _) => Ok(Some(auth.clone())),
            (None, Some(e)) => Err(e),
            (None, None) => Ok(None),
        }
    }
}

/// Given a URL, strip off the scheme and username if present.
fn strip_scheme_and_user(url: &Url) -> String {
    let url = url.as_str();
    // Find the position immediately after the '@' if a username is present.
    // or after the scheme otherwise.
    let pos = url
        .find('@')
        .map(|i| i + 1)
        .or_else(|| url.find("://").map(|i| i + 3));

    match pos {
        Some(i) => &url[i..],
        None => url,
    }
    .to_string()
}

#[cfg(test)]
mod test {
    use super::*;

    use configparser::config::Options;

    #[test]
    fn test_auth_groups() {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             foo.prefix = foo.com\n\
             foo.cert = /foo/cert\n\
             foo.key = /foo/key\n\
             foo.cacerts = /foo/cacerts\n\
             bar.prefix = bar.com\n\
             bar.cert = /bar/cert\n\
             bar.key = /bar/key\n\
             baz.cert = /baz/cert\n\
             baz.key = /baz/key\n\
             foo.username = user\n\
             foo.schemes = http https\n\
             foo.priority = 1\n
             ",
            &Options::default(),
        );
        let groups = AuthConfig::new(&config).groups;

        // Only 2 groups because "baz" is missing the required "prefix" setting.
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0],
            Auth {
                group: "foo".into(),
                prefix: "foo.com".into(),
                cert: Some("/foo/cert".into()),
                key: Some("/foo/key".into()),
                cacerts: Some("/foo/cacerts".into()),
                username: Some("user".into()),
                schemes: vec!["http".into(), "https".into()],
                priority: 1,
                extras: HashMap::new(),
            }
        );
        assert_eq!(
            groups[1],
            Auth {
                group: "bar".into(),
                prefix: "bar.com".into(),
                cert: Some("/bar/cert".into()),
                key: Some("/bar/key".into()),
                cacerts: None,
                username: None,
                schemes: vec!["https".into()],
                priority: 0,
                extras: HashMap::new(),
            }
        );
    }

    #[test]
    fn test_strip_scheme_and_user() -> Result<()> {
        let url = "https://example.com/".parse()?;
        let stripped = strip_scheme_and_user(&url);
        assert_eq!(stripped, "example.com/");

        let url = "https://user@example.com:433/some/path?query=1".parse()?;
        let stripped = strip_scheme_and_user(&url);
        assert_eq!(stripped, "example.com:433/some/path?query=1");

        Ok(())
    }

    #[test]
    fn test_auth_for_url() -> Result<()> {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             default.prefix = *\n\
             a.prefix = example.com\n\
             a.schemes = http https\n\
             b.prefix = foo.com/bar\n\
             c.prefix = foo.com/bar/baz\n\
             d.prefix = bar.com\n\
             d.priority = 1\n\
             e.prefix = bar.com\n\
             e.username = e_user\n\
             f.prefix = baz.com\n\
             f.username = f_user\n\
             g.prefix = baz.com\n\
             h.prefix = example.net\n\
             h.username = h_user\n\
             i.prefix = example.net\n\
             i.username = i_user\n\
             j.prefix = invalid.com\n\
             j.cert = /does/not/exist\n\
             ",
            &Options::default(),
        );
        let auth_config = AuthConfig::new(&config);

        // Basic case: an exact match.
        let auth = auth_config
            .auth_for_url(&"http://example.com".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "a");

        // Scheme mismatch.
        let auth = auth_config.auth_for_url(&"ftp://example.com".parse()?)?;
        assert!(auth.is_none());

        // Given URL's hosts matches a config prefix, but doesn't match
        // the entire prefix.
        let auth = auth_config
            .auth_for_url(&"https://foo.com.".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "default");

        // Matching the entire prefix works as expected.
        let auth = auth_config
            .auth_for_url(&"https://foo.com/bar".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "b");

        // A more specific prefix wins.
        let auth = auth_config
            .auth_for_url(&"https://foo.com/bar/baz".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "c");

        // Still matches even if the URL has other components in it.
        let auth = auth_config
            .auth_for_url(&"https://foo.com/bar/baz/dir?query=1#fragment".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "c");

        // There are two entries matching this prefix, but one has higher priority.
        let auth = auth_config
            .auth_for_url(&"https://bar.com".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "d");

        // Even if another entry has a username match, the higher priority should win.
        let auth = auth_config
            .auth_for_url(&"https://e_user@bar.com".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "d");

        // Even if no user is specified in the URL, the entry with a username should
        // take precedence all else being equal.
        let auth = auth_config
            .auth_for_url(&"https://baz.com".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "f");

        // If all else fails, later entries take precedence over earlier ones.
        // Even if no user is specified in the URL, the entry with a username should
        // take precedence all else being equal.
        let auth = auth_config
            .auth_for_url(&"https://example.net".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "i");

        // If the cert of key is missing, the entry shouldn't match.
        let auth = auth_config
            .auth_for_url(&"https://invalid.com".parse()?)?
            .unwrap();
        assert_eq!(auth.group, "default");

        Ok(())
    }

    #[test]
    fn test_extras() -> Result<()> {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             foo.prefix = foo.com\n\
             foo.username = user\n\
             foo.hello = world\n\
             foo.bar = baz\n\
             ",
            &Options::default(),
        );
        let auth_config = AuthConfig::new(&config);

        let auth = auth_config
            .auth_for_url(&"https://foo.com".parse()?)?
            .unwrap();

        assert_eq!(auth.extras.get("hello").unwrap(), "world");
        assert_eq!(auth.extras.get("bar").unwrap(), "baz");
        assert_eq!(auth.extras.get("username"), None);

        Ok(())
    }
}
