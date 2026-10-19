//! Human-friendly durations for config files: `"500ms"`, `"30s"`, `"10m"`,
//! `"2h"`, `"1d"`, or a bare integer number of seconds.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration that serialises as a short string such as `"10m"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Duration(pub std::time::Duration);

impl Duration {
    /// From whole seconds.
    pub const fn from_secs(s: u64) -> Self {
        Self(std::time::Duration::from_secs(s))
    }

    /// From milliseconds.
    pub const fn from_millis(ms: u64) -> Self {
        Self(std::time::Duration::from_millis(ms))
    }

    /// The standard-library duration.
    pub const fn get(self) -> std::time::Duration {
        self.0
    }
}

impl From<std::time::Duration> for Duration {
    fn from(d: std::time::Duration) -> Self {
        Self(d)
    }
}

impl From<Duration> for std::time::Duration {
    fn from(d: Duration) -> Self {
        d.0
    }
}

impl std::str::FromStr for Duration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let n: u64 = num
            .parse()
            .map_err(|_| format!("invalid duration {s:?}; use e.g. \"30s\", \"10m\" or \"2h\""))?;
        let ms = match unit {
            "ms" => Some(n),
            "" | "s" => n.checked_mul(1_000),
            "m" => n.checked_mul(60_000),
            "h" => n.checked_mul(3_600_000),
            "d" => n.checked_mul(86_400_000),
            _ => {
                return Err(format!(
                    "invalid duration unit {unit:?} in {s:?}; use ms, s, m, h or d"
                ));
            }
        };
        ms.map(Self::from_millis)
            .ok_or_else(|| format!("duration {s:?} is too large"))
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.as_millis();
        for (unit, size) in [
            ("d", 86_400_000),
            ("h", 3_600_000),
            ("m", 60_000),
            ("s", 1_000),
        ] {
            if ms >= size && ms.is_multiple_of(size) {
                return write!(f, "{}{unit}", ms / size);
            }
        }
        if ms == 0 {
            f.write_str("0s")
        } else {
            write!(f, "{ms}ms")
        }
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Secs(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Secs(s) => Ok(Self::from_secs(s)),
            Raw::Text(t) => t.parse().map_err(serde::de::Error::custom),
        }
    }
}

impl JsonSchema for Duration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Duration".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A duration such as \"500ms\", \"30s\", \"10m\", \"2h\" or \"1d\", or whole seconds.",
            "anyOf": [
                {"type": "string", "pattern": "^[0-9]+(ms|s|m|h|d)?$"},
                {"type": "integer", "minimum": 0}
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays() {
        for (text, ms, shown) in [
            ("500ms", 500, "500ms"),
            ("30s", 30_000, "30s"),
            ("90", 90_000, "90s"),
            ("120s", 120_000, "2m"),
            ("10m", 600_000, "10m"),
            ("2h", 7_200_000, "2h"),
            ("1d", 86_400_000, "1d"),
            ("0s", 0, "0s"),
        ] {
            let d: Duration = text.parse().unwrap();
            assert_eq!(d, Duration::from_millis(ms), "{text}");
            assert_eq!(d.to_string(), shown, "{text}");
        }
        for bad in ["", "m", "10x", "-1s", "1.5h"] {
            assert!(bad.parse::<Duration>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serde_string_or_integer() {
        #[derive(Deserialize, Serialize)]
        struct T {
            d: Duration,
        }
        let t: T = toml::from_str("d = \"10m\"").unwrap();
        assert_eq!(t.d, Duration::from_secs(600));
        let t: T = toml::from_str("d = 45").unwrap();
        assert_eq!(t.d, Duration::from_secs(45));
        assert_eq!(serde_json::to_string(&t).unwrap(), r#"{"d":"45s"}"#);
    }
}
