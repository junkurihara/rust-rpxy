use crate::constants::ACME_ACCOUNT_SUBDIR;
use async_trait::async_trait;
use aws_lc_rs as crypto;
use base64::prelude::*;
use blocking::unblock;
use crypto::digest::{Context, SHA256};
use rustls_acme::{AccountCache, CertCache};
use std::{
  io::ErrorKind,
  path::{Path, PathBuf},
};

enum FileType {
  Account,
  Cert,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DirCache {
  pub(super) account_dir: PathBuf,
  pub(super) cert_dir: PathBuf,
}

impl DirCache {
  pub fn new<P>(dir: P, server_name: &str) -> Self
  where
    P: AsRef<Path>,
  {
    Self {
      account_dir: dir.as_ref().join(ACME_ACCOUNT_SUBDIR),
      cert_dir: dir.as_ref().join(server_name),
    }
  }
  async fn read_if_exist(&self, file: impl AsRef<Path>, file_type: FileType) -> Result<Option<Vec<u8>>, std::io::Error> {
    let subdir = match file_type {
      FileType::Account => &self.account_dir,
      FileType::Cert => &self.cert_dir,
    };
    let file_path = subdir.join(file);
    match unblock(move || std::fs::read(file_path)).await {
      Ok(content) => Ok(Some(content)),
      Err(err) => match err.kind() {
        ErrorKind::NotFound => Ok(None),
        _ => Err(err),
      },
    }
  }
  async fn write(&self, file: impl AsRef<Path>, contents: impl AsRef<[u8]>, file_type: FileType) -> Result<(), std::io::Error> {
    let subdir = match file_type {
      FileType::Account => &self.account_dir,
      FileType::Cert => &self.cert_dir,
    }
    .clone();
    let subdir_clone = subdir.clone();
    unblock(move || std::fs::create_dir_all(subdir_clone)).await?;
    let file_path = subdir.join(file);
    let contents = contents.as_ref().to_owned();
    unblock(move || std::fs::write(file_path, contents)).await
  }
  pub fn cached_account_file_name(contact: &[String], directory_url: impl AsRef<str>) -> String {
    let mut ctx = Context::new(&SHA256);
    for el in contact {
      ctx.update(el.as_ref());
      ctx.update(&[0])
    }
    ctx.update(directory_url.as_ref().as_bytes());
    let hash = BASE64_URL_SAFE_NO_PAD.encode(ctx.finish());
    format!("cached_account_{}", hash)
  }
  pub fn cached_cert_file_name(domains: &[String], directory_url: impl AsRef<str>) -> String {
    let mut ctx = Context::new(&SHA256);
    for domain in domains {
      ctx.update(domain.as_ref());
      ctx.update(&[0])
    }
    ctx.update(directory_url.as_ref().as_bytes());
    let hash = BASE64_URL_SAFE_NO_PAD.encode(ctx.finish());
    format!("cached_cert_{}", hash)
  }
}

#[async_trait]
impl CertCache for DirCache {
  type EC = std::io::Error;
  async fn load_cert(&self, domains: &[String], directory_url: &str) -> Result<Option<Vec<u8>>, Self::EC> {
    let file_name = Self::cached_cert_file_name(domains, directory_url);
    self.read_if_exist(file_name, FileType::Cert).await
  }
  async fn store_cert(&self, domains: &[String], directory_url: &str, cert: &[u8]) -> Result<(), Self::EC> {
    let file_name = Self::cached_cert_file_name(domains, directory_url);
    self.write(file_name, cert, FileType::Cert).await
  }
}

#[async_trait]
impl AccountCache for DirCache {
  type EA = std::io::Error;
  async fn load_account(&self, contact: &[String], directory_url: &str) -> Result<Option<Vec<u8>>, Self::EA> {
    let file_name = Self::cached_account_file_name(contact, directory_url);
    self.read_if_exist(file_name, FileType::Account).await
  }

  async fn store_account(&self, contact: &[String], directory_url: &str, account: &[u8]) -> Result<(), Self::EA> {
    let file_name = Self::cached_account_file_name(contact, directory_url);
    self.write(file_name, account, FileType::Account).await
  }
}
