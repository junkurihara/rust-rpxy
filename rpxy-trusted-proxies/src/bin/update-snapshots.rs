use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use ipnet::IpNet;
use reqwest::blocking::Client;
use rpxy_trusted_proxies::{BuiltinTrustedProxyAlias, builtin_aliases};
use serde::Deserialize;
use std::{
  collections::{BTreeMap, BTreeSet},
  env, fs,
  path::PathBuf,
  time::Duration,
};

#[derive(Clone, Debug)]
struct Args {
  providers: Vec<BuiltinTrustedProxyAlias>,
  check: bool,
  timeout: Duration,
}

#[derive(Clone, Debug)]
struct SnapshotData {
  source_url: String,
  fetched_at: String,
  cidrs: Vec<String>,
}

#[derive(Clone, Debug)]
struct ProviderUpdateStatus {
  alias: BuiltinTrustedProxyAlias,
  changed: bool,
  cidr_count: usize,
  fetched_at: String,
}

fn main() -> Result<()> {
  let args = parse_args()?;
  let client = Client::builder()
    .timeout(args.timeout)
    .build()
    .context("failed to build HTTP client for snapshot updater")?;

  let current_date = Utc::now().format("%Y-%m-%d").to_string();
  let mut snapshots = BTreeMap::new();
  let mut statuses = Vec::new();

  for alias in builtin_aliases() {
    let data = if args.providers.contains(alias) {
      let fetched = fetch_snapshot(&client, *alias, &current_date)?;
      let current = current_snapshot(*alias);
      let changed = current.source_url != fetched.source_url || !same_cidrs(&current.cidrs, &fetched.cidrs);
      let snapshot = if changed {
        fetched
      } else {
        SnapshotData {
          source_url: current.source_url,
          fetched_at: current.fetched_at,
          cidrs: current.cidrs,
        }
      };
      statuses.push(ProviderUpdateStatus {
        alias: *alias,
        changed,
        cidr_count: snapshot.cidrs.len(),
        fetched_at: snapshot.fetched_at.clone(),
      });
      snapshot
    } else {
      current_snapshot(*alias)
    };
    snapshots.insert(*alias, data);
  }

  let rendered = render_snapshots_module(&snapshots)?;
  let snapshots_path = snapshots_file_path();
  let existing = fs::read_to_string(&snapshots_path).unwrap_or_default();

  if args.check {
    if existing == rendered {
      println!("snapshots are up to date");
      return Ok(());
    }
    bail!("snapshot file is outdated: {}", snapshots_path.display());
  }

  if existing == rendered {
    println!("no snapshot changes detected");
    return Ok(());
  }

  fs::write(&snapshots_path, rendered).with_context(|| format!("failed to write snapshot file {}", snapshots_path.display()))?;

  for status in &statuses {
    println!(
      "{} {} snapshot: {} CIDRs ({})",
      if status.changed { "updated" } else { "unchanged" },
      status.alias.as_str(),
      status.cidr_count,
      status.fetched_at
    );
  }

  Ok(())
}

fn parse_args() -> Result<Args> {
  let mut providers = Vec::new();
  let mut check = false;
  let mut timeout_seconds = 10_u64;
  let mut iter = env::args().skip(1);

  while let Some(arg) = iter.next() {
    match arg.as_str() {
      "--provider" => {
        let raw = iter.next().ok_or_else(|| anyhow!("missing value for --provider"))?;
        if raw.eq_ignore_ascii_case("all") {
          providers = builtin_aliases().to_vec();
        } else {
          let alias = raw.parse::<BuiltinTrustedProxyAlias>()?;
          if !providers.contains(&alias) {
            providers.push(alias);
          }
        }
      }
      "--check" => check = true,
      "--timeout-seconds" => {
        let raw = iter.next().ok_or_else(|| anyhow!("missing value for --timeout-seconds"))?;
        timeout_seconds = raw
          .parse::<u64>()
          .map_err(|e| anyhow!("invalid timeout seconds `{raw}`: {e}"))?;
      }
      "--help" | "-h" => {
        print_usage();
        std::process::exit(0);
      }
      _ => bail!("unknown argument: {arg}"),
    }
  }

  if providers.is_empty() {
    providers = builtin_aliases().to_vec();
  }

  Ok(Args {
    providers,
    check,
    timeout: Duration::from_secs(timeout_seconds),
  })
}

fn print_usage() {
  println!(
    "Usage: cargo run -p rpxy-trusted-proxies --features update-snapshots --bin update-snapshots -- [--provider <alias>|all] [--check] [--timeout-seconds <n>]"
  );
}

fn fetch_snapshot(client: &Client, alias: BuiltinTrustedProxyAlias, fetched_at: &str) -> Result<SnapshotData> {
  let cidrs = match alias {
    BuiltinTrustedProxyAlias::Cloudflare => fetch_cloudflare_snapshot(client)?,
    BuiltinTrustedProxyAlias::Cloudfront => fetch_cloudfront_snapshot(client)?,
    BuiltinTrustedProxyAlias::Fastly => fetch_fastly_snapshot(client)?,
  };

  Ok(SnapshotData {
    source_url: alias.source_url().to_string(),
    fetched_at: fetched_at.to_string(),
    cidrs,
  })
}

fn current_snapshot(alias: BuiltinTrustedProxyAlias) -> SnapshotData {
  SnapshotData {
    source_url: alias.source_url().to_string(),
    fetched_at: alias.fetched_at().to_string(),
    cidrs: alias.cidr_strings().iter().map(|cidr| (*cidr).to_string()).collect(),
  }
}

fn fetch_cloudflare_snapshot(client: &Client) -> Result<Vec<String>> {
  #[derive(Deserialize)]
  struct CloudflareResult {
    ipv4_cidrs: Vec<String>,
    ipv6_cidrs: Vec<String>,
  }

  #[derive(Deserialize)]
  struct CloudflareResponse {
    result: CloudflareResult,
    success: bool,
  }

  let response = client
    .get(BuiltinTrustedProxyAlias::Cloudflare.source_url())
    .send()
    .context("failed to fetch Cloudflare IP ranges")?
    .error_for_status()
    .context("Cloudflare IP range endpoint returned an error status")?
    .json::<CloudflareResponse>()
    .context("failed to decode Cloudflare IP range response")?;

  if !response.success {
    bail!("Cloudflare IP range endpoint reported an unsuccessful response");
  }

  let cidrs = response
    .result
    .ipv4_cidrs
    .into_iter()
    .chain(response.result.ipv6_cidrs)
    .collect::<Vec<_>>();
  normalize_cidrs(cidrs)
}

fn fetch_cloudfront_snapshot(client: &Client) -> Result<Vec<String>> {
  #[derive(Deserialize)]
  struct CloudfrontResponse {
    #[serde(rename = "CLOUDFRONT_GLOBAL_IP_LIST")]
    cloudfront_global_ip_list: Vec<String>,
  }

  let response = client
    .get(BuiltinTrustedProxyAlias::Cloudfront.source_url())
    .send()
    .context("failed to fetch CloudFront IP ranges")?
    .error_for_status()
    .context("CloudFront IP range endpoint returned an error status")?
    .json::<CloudfrontResponse>()
    .context("failed to decode CloudFront IP range response")?;

  normalize_cidrs(response.cloudfront_global_ip_list)
}

fn fetch_fastly_snapshot(client: &Client) -> Result<Vec<String>> {
  #[derive(Deserialize)]
  struct FastlyResponse {
    addresses: Vec<String>,
    ipv6_addresses: Vec<String>,
  }

  let response = client
    .get(BuiltinTrustedProxyAlias::Fastly.source_url())
    .send()
    .context("failed to fetch Fastly IP ranges")?
    .error_for_status()
    .context("Fastly IP range endpoint returned an error status")?
    .json::<FastlyResponse>()
    .context("failed to decode Fastly IP range response")?;

  let cidrs = response
    .addresses
    .into_iter()
    .chain(response.ipv6_addresses)
    .collect::<Vec<_>>();
  normalize_cidrs(cidrs)
}

fn normalize_cidrs(cidrs: Vec<String>) -> Result<Vec<String>> {
  let mut parsed = cidrs
    .into_iter()
    .map(|cidr| {
      cidr
        .parse::<IpNet>()
        .map_err(|e| anyhow!("invalid CIDR returned by provider `{cidr}`: {e}"))
    })
    .collect::<Result<Vec<_>>>()?;
  parsed.sort_by_key(|cidr| cidr.to_string());
  parsed.dedup();
  Ok(parsed.into_iter().map(|cidr| cidr.to_string()).collect())
}

fn same_cidrs(current: &[String], fetched: &[String]) -> bool {
  let current = current.iter().map(String::as_str).collect::<BTreeSet<_>>();
  let fetched = fetched.iter().map(String::as_str).collect::<BTreeSet<_>>();
  current == fetched
}

fn render_snapshots_module(snapshots: &BTreeMap<BuiltinTrustedProxyAlias, SnapshotData>) -> Result<String> {
  let mut output = String::from("use super::BuiltinTrustedProxyAlias;\n\n");
  output.push_str("// Generated by `cargo run -p rpxy-trusted-proxies --features update-snapshots --bin update-snapshots`.\n");
  output.push_str("// Do not edit by hand unless you are intentionally adjusting a snapshot.\n\n");
  output.push_str("pub const BUILTIN_ALIASES: &[BuiltinTrustedProxyAlias] = &[\n");
  for alias in builtin_aliases() {
    output.push_str(&format!("  BuiltinTrustedProxyAlias::{},\n", rust_variant_name(*alias)));
  }
  output.push_str("];\n\n");

  for alias in builtin_aliases() {
    let snapshot = snapshots
      .get(alias)
      .ok_or_else(|| anyhow!("missing snapshot data for {}", alias.as_str()))?;
    let prefix = constant_prefix(*alias);
    output.push_str(&format!("pub const {prefix}_SOURCE_URL: &str = {:?};\n", snapshot.source_url));
    output.push_str(&format!(
      "pub const {prefix}_FETCHED_AT: &str = {:?};\n\n",
      snapshot.fetched_at
    ));
    output.push_str(&format!("pub const {prefix}_CIDRS: &[&str] = &[\n"));
    for cidr in &snapshot.cidrs {
      output.push_str(&format!("  {:?},\n", cidr));
    }
    output.push_str("];\n\n");
  }

  Ok(output)
}

fn constant_prefix(alias: BuiltinTrustedProxyAlias) -> &'static str {
  match alias {
    BuiltinTrustedProxyAlias::Cloudflare => "CLOUDFLARE",
    BuiltinTrustedProxyAlias::Cloudfront => "CLOUDFRONT",
    BuiltinTrustedProxyAlias::Fastly => "FASTLY",
  }
}

fn rust_variant_name(alias: BuiltinTrustedProxyAlias) -> &'static str {
  match alias {
    BuiltinTrustedProxyAlias::Cloudflare => "Cloudflare",
    BuiltinTrustedProxyAlias::Cloudfront => "Cloudfront",
    BuiltinTrustedProxyAlias::Fastly => "Fastly",
  }
}

fn snapshots_file_path() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("snapshots.rs")
}
