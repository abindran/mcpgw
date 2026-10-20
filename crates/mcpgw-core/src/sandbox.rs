//! Sandbox profiles and resource limits (M3-02).
//!
//! A container server runs under a [`Sandbox`] profile, `standard` by
//! default, which drops privileges and caps memory and processes. [`Limits`]
//! set on the server override the profile's defaults.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A byte count written like `"256MiB"`, `"512m"`, `"1GiB"` or a bare integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(pub u64);

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

impl ByteSize {
    /// From mebibytes.
    pub const fn mib(n: u64) -> Self {
        Self(n * MIB)
    }

    /// Bytes.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::str::FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let n: u64 = num
            .parse()
            .map_err(|_| format!("invalid size {s:?}; use e.g. \"256MiB\" or \"1GiB\""))?;
        let mult = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "k" | "kb" | "kib" => KIB,
            "m" | "mb" | "mib" => MIB,
            "g" | "gb" | "gib" => GIB,
            other => {
                return Err(format!(
                    "invalid size unit {other:?} in {s:?}; use KiB, MiB or GiB"
                ));
            }
        };
        n.checked_mul(mult)
            .map(Self)
            .ok_or_else(|| format!("size {s:?} is too large"))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (unit, size) in [("GiB", GIB), ("MiB", MIB), ("KiB", KIB)] {
            if self.0 >= size && self.0.is_multiple_of(size) {
                return write!(f, "{}{unit}", self.0 / size);
            }
        }
        write!(f, "{}", self.0)
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bytes(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bytes(b) => Ok(Self(b)),
            Raw::Text(t) => t.parse().map_err(serde::de::Error::custom),
        }
    }
}

impl JsonSchema for ByteSize {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ByteSize".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A size such as \"256MiB\", \"512m\" or \"1GiB\", or bytes.",
            "anyOf": [
                {"type": "string", "pattern": "^[0-9]+ ?([kKmMgG]i?[bB]?|[bB])?$"},
                {"type": "integer", "minimum": 0}
            ]
        })
    }
}

/// Resource limits for a server.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Memory cap; exceeding it kills the server (no swap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<ByteSize>,
    /// CPU cap in cores, e.g. `0.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    /// Most processes (and threads) at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u32>,
}

impl Limits {
    /// No limit set.
    pub fn is_empty(&self) -> bool {
        self.memory.is_none() && self.cpus.is_none() && self.pids.is_none()
    }

    /// These limits, with unset ones taken from `defaults`.
    pub fn or(&self, defaults: &Limits) -> Limits {
        Limits {
            memory: self.memory.or(defaults.memory),
            cpus: self.cpus.or(defaults.cpus),
            pids: self.pids.or(defaults.pids),
        }
    }

    /// Check the values are usable.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(m) = self.memory
            && m.get() < 6 * MIB
        {
            return Err(format!("memory limit {m} is below the 6MiB minimum"));
        }
        if let Some(c) = self.cpus
            && !(c.is_finite() && c > 0.0)
        {
            return Err(format!("cpus = {c} must be a positive number"));
        }
        if self.pids == Some(0) {
            return Err("pids must be at least 1".into());
        }
        Ok(())
    }
}

/// How tightly a container server is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Sandbox {
    /// No restrictions beyond the server's own [`Limits`].
    Off,
    /// All capabilities dropped, no privilege escalation, 1 GiB memory and
    /// 512 processes unless set.
    #[default]
    Standard,
    /// `standard`, plus a read-only root filesystem with a 64 MiB tmpfs at
    /// `/tmp`, running as uid/gid 65534 (nobody), and defaults of 512 MiB,
    /// 1 CPU and 128 processes.
    Strict,
}

impl Sandbox {
    /// Limits applied when the server sets none.
    pub fn default_limits(self) -> Limits {
        match self {
            Sandbox::Off => Limits::default(),
            Sandbox::Standard => Limits {
                memory: Some(ByteSize(GIB)),
                cpus: None,
                pids: Some(512),
            },
            Sandbox::Strict => Limits {
                memory: Some(ByteSize::mib(512)),
                cpus: Some(1.0),
                pids: Some(128),
            },
        }
    }

    /// Whether this is the default profile (for serialisation).
    pub fn is_default(&self) -> bool {
        *self == Sandbox::default()
    }

    /// `docker run` / `podman run` flags for this profile and `limits`
    /// (already merged with the profile's defaults).
    pub fn run_flags(self, limits: &Limits) -> Vec<String> {
        let mut f: Vec<String> = Vec::new();
        if self != Sandbox::Off {
            f.extend(["--cap-drop=ALL", "--security-opt=no-new-privileges"].map(String::from));
        }
        if self == Sandbox::Strict {
            f.extend(
                [
                    "--read-only",
                    "--tmpfs=/tmp:rw,nosuid,nodev,size=64m",
                    "--user=65534:65534",
                ]
                .map(String::from),
            );
        }
        if let Some(m) = limits.memory {
            // the same value for swap: no swap, so going over is a kill
            f.push(format!("--memory={}", m.get()));
            f.push(format!("--memory-swap={}", m.get()));
        }
        if let Some(c) = limits.cpus {
            f.push(format!("--cpus={c}"));
        }
        if let Some(p) = limits.pids {
            f.push(format!("--pids-limit={p}"));
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        for (text, bytes, shown) in [
            ("256MiB", 256 * MIB, "256MiB"),
            ("512m", 512 * MIB, "512MiB"),
            ("1g", GIB, "1GiB"),
            ("1 GiB", GIB, "1GiB"),
            ("64k", 64 * KIB, "64KiB"),
            ("1000", 1000, "1000"),
        ] {
            let b: ByteSize = text.parse().unwrap();
            assert_eq!((b.get(), b.to_string().as_str()), (bytes, shown), "{text}");
        }
        assert!("12x".parse::<ByteSize>().is_err());
        assert!("MiB".parse::<ByteSize>().is_err());
        let l: Limits = toml::from_str("memory = \"256MiB\"\ncpus = 0.5\npids = 64").unwrap();
        assert_eq!(l.memory, Some(ByteSize::mib(256)));
        assert_eq!(
            toml::from_str::<Limits>(&toml::to_string(&l).unwrap()).unwrap(),
            l
        );
        assert!(toml::from_str::<Limits>("mem = 1").is_err());
    }

    #[test]
    fn limits_validate_and_merge() {
        let bad = |l: Limits| l.validate().unwrap_err();
        assert!(
            bad(Limits {
                memory: Some(ByteSize::mib(1)),
                ..Default::default()
            })
            .contains("6MiB")
        );
        assert!(
            bad(Limits {
                cpus: Some(0.0),
                ..Default::default()
            })
            .contains("positive")
        );
        assert!(
            bad(Limits {
                pids: Some(0),
                ..Default::default()
            })
            .contains("at least 1")
        );
        let mine = Limits {
            memory: Some(ByteSize::mib(256)),
            ..Default::default()
        };
        let eff = mine.or(&Sandbox::Strict.default_limits());
        assert_eq!(
            (eff.memory, eff.cpus, eff.pids),
            (Some(ByteSize::mib(256)), Some(1.0), Some(128))
        );
    }

    #[test]
    fn profile_flags() {
        let std_flags = Sandbox::Standard.run_flags(&Sandbox::Standard.default_limits());
        assert_eq!(
            std_flags,
            [
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--memory=1073741824",
                "--memory-swap=1073741824",
                "--pids-limit=512"
            ]
        );
        let strict = Sandbox::Strict.run_flags(&Sandbox::Strict.default_limits());
        for f in [
            "--read-only",
            "--user=65534:65534",
            "--cpus=1",
            "--pids-limit=128",
        ] {
            assert!(strict.iter().any(|x| x == f), "{f}: {strict:?}");
        }
        assert!(Sandbox::Off.run_flags(&Limits::default()).is_empty());
        assert_eq!(serde_json::to_value(Sandbox::Strict).unwrap(), "strict");
    }
}
