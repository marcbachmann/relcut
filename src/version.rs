use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Patch,
    Minor,
    Major,
}

impl Bump {
    pub fn parse(s: &str) -> Result<Bump, String> {
        match s {
            "patch" => Ok(Bump::Patch),
            "minor" => Ok(Bump::Minor),
            "major" => Ok(Bump::Major),
            _ => Err(format!("min-bump takes patch, minor or major, not '{s}'")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Bump::Patch => "patch",
            Bump::Minor => "minor",
            Bump::Major => "major",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const FIRST: Version = Version {
        major: 1,
        minor: 0,
        patch: 0,
    };

    // Only plain x.y.z: a prerelease or build suffix is not a release here.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty()
                || (p.len() > 1 && p.starts_with('0'))
                || !p.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            p.parse().ok()
        };
        let v = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(v)
    }

    pub fn bump(self, bump: Bump) -> Version {
        match bump {
            Bump::Major => Version {
                major: self.major + 1,
                minor: 0,
                patch: 0,
            },
            Bump::Minor => Version {
                minor: self.minor + 1,
                patch: 0,
                ..self
            },
            Bump::Patch => Version {
                patch: self.patch + 1,
                ..self
            },
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// `v1` pins the major, `v1.1` the minor as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraint {
    major: u64,
    minor: Option<u64>,
}

impl Constraint {
    pub fn parse(s: &str) -> Result<Constraint, String> {
        let body = s.trim().trim_start_matches('v');
        let invalid = || format!("constraint must look like v1 or v1.1, not '{s}'");
        let mut parts = body.split('.');
        let num = |p: Option<&str>| p.and_then(|p| p.parse::<u64>().ok());
        let major = num(parts.next()).ok_or_else(invalid)?;
        let minor = match parts.next() {
            None => None,
            p => Some(num(p).ok_or_else(invalid)?),
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(Constraint { major, minor })
    }

    pub fn allows(&self, v: Version) -> bool {
        v.major == self.major && self.minor.is_none_or(|m| v.minor == m)
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.minor {
            Some(m) => write!(f, "v{}.{}", self.major, m),
            None => write!(f, "v{}", self.major),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_versions_only() {
        assert_eq!(
            Version::parse("1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        for bad in ["1.2", "1.2.3.4", "1.2.3-beta.1", "01.2.3", "a.b.c", ""] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn bumps() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(v.bump(Bump::Major).to_string(), "2.0.0");
        assert_eq!(v.bump(Bump::Minor).to_string(), "1.3.0");
        assert_eq!(v.bump(Bump::Patch).to_string(), "1.2.4");
    }

    #[test]
    fn constraint_pins_major_or_minor() {
        let v = |s| Version::parse(s).unwrap();
        let major = Constraint::parse("v1").unwrap();
        assert!(major.allows(v("1.9.0")));
        assert!(!major.allows(v("2.0.0")));
        let minor = Constraint::parse("v312.0").unwrap();
        assert!(minor.allows(v("312.0.14")));
        assert!(!minor.allows(v("312.1.0")));
        for bad in ["", "v", "vx", "v1.x", "v1.1.1"] {
            assert!(Constraint::parse(bad).is_err(), "{bad}");
        }
    }
}
