mod snapshots;

use anyhow::{Result, anyhow};
use ipnet::IpNet;
use std::{collections::HashSet, str::FromStr};

/// Built-in alias names for trusted proxy providers.
///
/// The CIDR snapshots in this crate are intentionally static and are expected to
/// be refreshed in future releases or by an explicit snapshot update workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinTrustedProxyAlias {
  Cloudflare,
  Cloudfront,
  Fastly,
}

impl BuiltinTrustedProxyAlias {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Cloudflare => "cloudflare",
      Self::Cloudfront => "cloudfront",
      Self::Fastly => "fastly",
    }
  }

  pub fn source_url(self) -> &'static str {
    match self {
      Self::Cloudflare => snapshots::CLOUDFLARE_SOURCE_URL,
      Self::Cloudfront => snapshots::CLOUDFRONT_SOURCE_URL,
      Self::Fastly => snapshots::FASTLY_SOURCE_URL,
    }
  }

  pub fn fetched_at(self) -> &'static str {
    match self {
      Self::Cloudflare => snapshots::CLOUDFLARE_FETCHED_AT,
      Self::Cloudfront => snapshots::CLOUDFRONT_FETCHED_AT,
      Self::Fastly => snapshots::FASTLY_FETCHED_AT,
    }
  }

  pub fn cidr_strings(self) -> &'static [&'static str] {
    match self {
      Self::Cloudflare => snapshots::CLOUDFLARE_CIDRS,
      Self::Cloudfront => snapshots::CLOUDFRONT_CIDRS,
      Self::Fastly => snapshots::FASTLY_CIDRS,
    }
  }
}

impl FromStr for BuiltinTrustedProxyAlias {
  type Err = anyhow::Error;

  fn from_str(value: &str) -> Result<Self> {
    match value.trim().to_ascii_lowercase().as_str() {
      "cloudflare" => Ok(Self::Cloudflare),
      "cloudfront" => Ok(Self::Cloudfront),
      "fastly" => Ok(Self::Fastly),
      _ => Err(anyhow!("unknown trusted proxy alias: {value}")),
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustedProxyEntry {
  Cidr(IpNet),
  Alias(BuiltinTrustedProxyAlias),
}

impl FromStr for TrustedProxyEntry {
  type Err = anyhow::Error;

  fn from_str(value: &str) -> Result<Self> {
    if let Ok(alias) = value.parse::<BuiltinTrustedProxyAlias>() {
      return Ok(Self::Alias(alias));
    }
    let cidr = value
      .parse::<IpNet>()
      .map_err(|e| anyhow!("invalid trusted proxy entry `{value}`: {e}"))?;
    Ok(Self::Cidr(cidr))
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionReport {
  pub cidrs: Vec<IpNet>,
  pub expanded_aliases: Vec<BuiltinTrustedProxyAlias>,
}

pub fn builtin_aliases() -> &'static [BuiltinTrustedProxyAlias] {
  snapshots::BUILTIN_ALIASES
}

pub fn resolve_trusted_proxy_entries<I, S>(entries: I) -> Result<ResolutionReport>
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  let mut cidrs = Vec::new();
  let mut seen = HashSet::new();
  let mut expanded_aliases = Vec::new();

  for raw in entries {
    match raw.as_ref().parse::<TrustedProxyEntry>()? {
      TrustedProxyEntry::Cidr(cidr) => {
        if seen.insert(cidr) {
          cidrs.push(cidr);
        }
      }
      TrustedProxyEntry::Alias(alias) => {
        if !expanded_aliases.contains(&alias) {
          expanded_aliases.push(alias);
        }
        for cidr in parse_alias_cidrs(alias)? {
          if seen.insert(cidr) {
            cidrs.push(cidr);
          }
        }
      }
    }
  }

  Ok(ResolutionReport { cidrs, expanded_aliases })
}

fn parse_alias_cidrs(alias: BuiltinTrustedProxyAlias) -> Result<Vec<IpNet>> {
  alias
    .cidr_strings()
    .iter()
    .map(|cidr| {
      cidr
        .parse::<IpNet>()
        .map_err(|e| anyhow!("invalid built-in CIDR for alias {}: {}: {}", alias.as_str(), cidr, e))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_alias_and_cidr_entries() {
    assert_eq!(
      "cloudflare".parse::<TrustedProxyEntry>().unwrap(),
      TrustedProxyEntry::Alias(BuiltinTrustedProxyAlias::Cloudflare)
    );
    assert_eq!(
      "10.0.0.0/8".parse::<TrustedProxyEntry>().unwrap(),
      TrustedProxyEntry::Cidr("10.0.0.0/8".parse().unwrap())
    );
  }

  #[test]
  fn rejects_unknown_alias() {
    let err = "example-cdn".parse::<TrustedProxyEntry>().unwrap_err();
    assert!(err.to_string().contains("invalid trusted proxy entry"));
  }

  #[test]
  fn resolves_mixed_entries_with_dedup() {
    let report = resolve_trusted_proxy_entries([
      "10.0.0.0/8",
      "cloudflare",
      "cloudfront",
      "10.0.0.0/8",
      "fastly",
      "cloudflare",
    ])
    .unwrap();

    assert!(report.cidrs.contains(&"10.0.0.0/8".parse().unwrap()));
    assert!(report.cidrs.contains(&"173.245.48.0/20".parse().unwrap()));
    assert!(report.cidrs.contains(&"120.52.22.96/27".parse().unwrap()));
    assert!(report.cidrs.contains(&"23.235.32.0/20".parse().unwrap()));
    assert_eq!(
      report.expanded_aliases,
      vec![
        BuiltinTrustedProxyAlias::Cloudflare,
        BuiltinTrustedProxyAlias::Cloudfront,
        BuiltinTrustedProxyAlias::Fastly,
      ]
    );
  }

  #[test]
  fn exposes_builtin_aliases() {
    assert_eq!(
      builtin_aliases(),
      &[
        BuiltinTrustedProxyAlias::Cloudflare,
        BuiltinTrustedProxyAlias::Cloudfront,
        BuiltinTrustedProxyAlias::Fastly,
      ]
    );
  }
}
