//! Snapshot packaging on the filesystem (ADR 0001 decisions B/C/F,
//! amendment A3).
//!
//! A snapshot directory holds `manifest.json` (the T1 manifest: no
//! ciphertext, no secrets), a `payload/` tree (database export,
//! `secrets/`, environment file) and `checksums.sha256` with a SHA-256
//! of every payload file. A snapshot is only ever promoted by rename
//! after verification, the previous verified snapshot is kept, and
//! everything is created owner-only (0600 files, 0700 directories on
//! Unix; the Windows ACL equivalent is an operator concern per A3).
//! Log lines carry relative paths, hashes and byte counts only — never
//! file contents — so a secret's value can never reach a log.

use crate::manifest::SnapshotManifest;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const CHECKSUMS_FILE: &str = "checksums.sha256";
pub const PAYLOAD_DIR: &str = "payload";

#[derive(Debug)]
pub enum SnapshotError {
    Io(io::Error),
    MissingManifest(PathBuf),
    InvalidManifest(String),
    MissingChecksums(PathBuf),
    InvalidChecksums(String),
    MissingFile(String),
    UnlistedFile(String),
    Symlink(String),
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    EmptyCiphertextHash,
    CiphertextHashMismatch {
        expected: String,
        actual: String,
    },
    InvalidNodeLocalSecret(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "io error: {e}"),
            SnapshotError::MissingManifest(p) => {
                write!(f, "missing snapshot manifest at {}", p.display())
            }
            SnapshotError::InvalidManifest(m) => write!(f, "invalid snapshot manifest: {m}"),
            SnapshotError::MissingChecksums(p) => {
                write!(f, "missing checksums file at {}", p.display())
            }
            SnapshotError::InvalidChecksums(m) => write!(f, "invalid checksums file: {m}"),
            SnapshotError::MissingFile(p) => write!(f, "snapshot file missing: {p}"),
            SnapshotError::UnlistedFile(p) => {
                write!(f, "payload file not listed in checksums: {p}")
            }
            SnapshotError::Symlink(p) => {
                write!(f, "snapshot payload contains a symlink: {p}")
            }
            SnapshotError::ChecksumMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "checksum mismatch for {path}: expected {expected}, got {actual}"
                )
            }
            SnapshotError::EmptyCiphertextHash => {
                write!(f, "ciphertext hash is empty in snapshot manifest")
            }
            SnapshotError::CiphertextHashMismatch { expected, actual } => {
                write!(
                    f,
                    "ciphertext hash mismatch: expected {expected}, got {actual}"
                )
            }
            SnapshotError::InvalidNodeLocalSecret(name) => {
                write!(
                    f,
                    "invalid node-local secret name: {name} (plain file name required)"
                )
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, SnapshotError>;

/// The operator-surface inputs of one snapshot: an `install.sh
/// db-export` file, the instance `secrets/` directory, and the
/// environment configuration file (ADR decision C).
#[derive(Debug, Clone)]
pub struct SnapshotSources {
    pub database_export: PathBuf,
    pub secrets_dir: PathBuf,
    pub environment_file: PathBuf,
}

/// Secret file names that are node-local by default (ADR 0001
/// decision C): they belong to the node itself, so snapshot creation
/// never replicates them and applying a snapshot can therefore never
/// overwrite a standby's own copy. `postgres_password` is the node's
/// own Postgres cluster password: a logical `pg_dump` restore does
/// not change a cluster's password, so the primary's copy must never
/// travel.
pub const DEFAULT_NODE_LOCAL_SECRETS: &[&str] = &["postgres_password"];

fn sha256_hex_of_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn sha256_hex_of_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn create_dir_owner_only(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    set_mode(path, 0o700)
}

fn copy_file_owner_only(src: &Path, dst: &Path) -> Result<u64> {
    if let Some(parent) = dst.parent() {
        create_dir_owner_only(parent)?;
    }
    let bytes = fs::copy(src, dst)?;
    set_mode(dst, 0o600)?;
    Ok(bytes)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u64> {
    create_dir_owner_only(dst)?;
    let mut total = 0u64;
    let mut entries: Vec<PathBuf> = fs::read_dir(src)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    entries.sort();
    for entry in entries {
        let name = entry.file_name().expect("dir entry has a file name");
        let target = dst.join(name);
        if entry.is_dir() {
            total += copy_dir_recursive(&entry, &target)?;
        } else {
            total += copy_file_owner_only(&entry, &target)?;
        }
    }
    Ok(total)
}

/// Copy the instance `secrets/` directory into a snapshot, skipping
/// top-level files named in `node_local` (ADR decision C: those files
/// belong to the node, are never replicated, and so can never
/// overwrite a standby's own copy when the snapshot is applied).
/// Skips are logged by file name only; contents are never logged.
fn copy_secrets_dir(
    src: &Path,
    dst: &Path,
    node_local: &BTreeSet<&str>,
    log: &mut dyn FnMut(&str),
) -> Result<u64> {
    create_dir_owner_only(dst)?;
    let mut total = 0u64;
    let mut entries: Vec<PathBuf> = fs::read_dir(src)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    entries.sort();
    for entry in entries {
        let name = entry.file_name().expect("dir entry has a file name");
        let target = dst.join(name);
        if entry.is_dir() {
            total += copy_dir_recursive(&entry, &target)?;
        } else if node_local.contains(name.to_string_lossy().as_ref()) {
            log(&format!(
                "snapshot: secrets/{} is node-local, not replicated",
                name.to_string_lossy()
            ));
        } else {
            total += copy_file_owner_only(&entry, &target)?;
        }
    }
    Ok(total)
}

/// All payload files under `root`, as sorted root-relative paths using
/// `/` separators (the checksum file format is platform-independent).
fn list_payload_files(root: &Path) -> Result<Vec<String>> {
    let payload = root.join(PAYLOAD_DIR);
    let mut out = Vec::new();
    if payload.is_dir() {
        collect_files(&payload, &payload, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    entries.sort();
    for entry in entries {
        let file_type = fs::symlink_metadata(&entry)?.file_type();
        if file_type.is_symlink() {
            let rel = entry.strip_prefix(base).expect("entry is under base");
            return Err(SnapshotError::Symlink(format!(
                "{PAYLOAD_DIR}/{}",
                rel.to_string_lossy().replace('\\', "/")
            )));
        }
        if file_type.is_dir() {
            collect_files(base, &entry, out)?;
        } else {
            let rel = entry.strip_prefix(base).expect("entry is under base");
            out.push(format!(
                "{PAYLOAD_DIR}/{}",
                rel.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    Ok(())
}

/// Reject any symlink inside the snapshot payload at `root` (T7a):
/// a symlinked payload entry would make verify/stage follow it and
/// materialise content from outside the snapshot. Uses
/// `symlink_metadata` throughout, never follows a link, and the
/// error names only the root-relative path.
fn reject_payload_symlinks(root: &Path) -> Result<()> {
    let payload = root.join(PAYLOAD_DIR);
    let meta = match fs::symlink_metadata(&payload) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return Err(SnapshotError::Symlink(PAYLOAD_DIR.to_string()));
    }
    if meta.is_dir() {
        reject_symlinks_in_dir(root, &payload)?;
    }
    Ok(())
}

fn reject_symlinks_in_dir(root: &Path, dir: &Path) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    entries.sort();
    for entry in entries {
        let file_type = fs::symlink_metadata(&entry)?.file_type();
        if file_type.is_symlink() {
            let rel = entry.strip_prefix(root).expect("entry is under root");
            return Err(SnapshotError::Symlink(
                rel.to_string_lossy().replace('\\', "/"),
            ));
        }
        if file_type.is_dir() {
            reject_symlinks_in_dir(root, &entry)?;
        }
    }
    Ok(())
}

fn compute_checksums(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for rel in list_payload_files(root)? {
        let hash = sha256_hex_of_file(&root.join(&rel))?;
        map.insert(rel, hash);
    }
    Ok(map)
}

fn write_checksums(root: &Path, checksums: &BTreeMap<String, String>) -> Result<()> {
    let mut text = String::new();
    for (path, hash) in checksums {
        text.push_str(&format!("{hash}  {path}\n"));
    }
    let file = root.join(CHECKSUMS_FILE);
    fs::write(&file, text)?;
    set_mode(&file, 0o600)
}

/// Validate one entry path from `checksums.sha256` (T7b): it must
/// be a relative path of normal components only, so a crafted
/// checksums file can never make verify read outside the snapshot
/// directory. Rejects `..`, absolute paths, empty and `.`
/// components, backslash separators and NUL; the error names the
/// offending entry.
fn validate_checksum_path(path: &str) -> Result<()> {
    let reject = || SnapshotError::InvalidChecksums(format!("unsafe entry path: {path}"));
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return Err(reject());
    }
    for part in path.split('/') {
        match part {
            "" | "." | ".." => return Err(reject()),
            _ => {}
        }
    }
    Ok(())
}

fn read_checksums(root: &Path) -> Result<BTreeMap<String, String>> {
    let file = root.join(CHECKSUMS_FILE);
    if !file.is_file() {
        return Err(SnapshotError::MissingChecksums(file));
    }
    let text = fs::read_to_string(&file)?;
    let mut map = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (hash, path) = line.split_once("  ").ok_or_else(|| {
            SnapshotError::InvalidChecksums(format!("line {} has no separator", i + 1))
        })?;
        validate_checksum_path(path)?;
        map.insert(path.to_string(), hash.to_string());
    }
    Ok(map)
}

/// Create a snapshot directory at `dest` from `sources` and `manifest`,
/// then verify it. Any previous content at `dest` is replaced. Log
/// lines report relative paths, hashes and byte counts only.
pub fn create_snapshot(
    sources: &SnapshotSources,
    manifest: &SnapshotManifest,
    dest: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    create_snapshot_with_node_local(sources, manifest, dest, log, &[])
}

/// Create a snapshot like [`create_snapshot`], additionally treating
/// `extra_node_local` secret file names as node-local. The default
/// node-local set ([`DEFAULT_NODE_LOCAL_SECRETS`]) always applies:
/// extras only add to it, they can never un-exclude a default. Each
/// extra must be a plain file name, not a path; an invalid name is
/// rejected before `dest` is touched.
pub fn create_snapshot_with_node_local(
    sources: &SnapshotSources,
    manifest: &SnapshotManifest,
    dest: &Path,
    log: &mut dyn FnMut(&str),
    extra_node_local: &[&str],
) -> Result<()> {
    let mut node_local: BTreeSet<&str> = DEFAULT_NODE_LOCAL_SECRETS.iter().copied().collect();
    for name in extra_node_local {
        if name.is_empty()
            || *name == "."
            || *name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(SnapshotError::InvalidNodeLocalSecret((*name).to_string()));
        }
        node_local.insert(name);
    }
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    create_dir_owner_only(dest)?;
    let payload = dest.join(PAYLOAD_DIR);
    create_dir_owner_only(&payload)?;
    let db_name = sources
        .database_export
        .file_name()
        .expect("database export has a file name");
    let db_bytes = copy_file_owner_only(
        &sources.database_export,
        &payload.join("database").join(db_name),
    )?;
    log(&format!("snapshot: database export {} bytes", db_bytes));
    let secret_bytes = copy_secrets_dir(
        &sources.secrets_dir,
        &payload.join("secrets"),
        &node_local,
        log,
    )?;
    log(&format!(
        "snapshot: secrets directory {} bytes (contents never logged)",
        secret_bytes
    ));
    let env_name = sources
        .environment_file
        .file_name()
        .expect("environment file has a file name");
    let env_bytes = copy_file_owner_only(
        &sources.environment_file,
        &payload.join("environment").join(env_name),
    )?;
    log(&format!("snapshot: environment file {} bytes", env_bytes));
    let manifest_json = serde_json::to_string_pretty(manifest)
        .map_err(|e| SnapshotError::InvalidManifest(e.to_string()))?;
    let manifest_path = dest.join(MANIFEST_FILE);
    fs::write(&manifest_path, manifest_json)?;
    set_mode(&manifest_path, 0o600)?;
    let checksums = compute_checksums(dest)?;
    for (path, hash) in &checksums {
        log(&format!("snapshot: {path} sha256 {hash}"));
    }
    write_checksums(dest, &checksums)?;
    verify_snapshot(dest, log)?;
    log("snapshot: created and verified");
    Ok(())
}

/// Verify a snapshot directory: the manifest parses, every checksummed
/// payload file is present with a matching SHA-256 (a corrupted or
/// truncated file fails here), no payload file is unlisted, and the
/// manifest ciphertext hash is bound to the payload: it must be
/// non-empty and equal the SHA-256 fingerprint of the packaged
/// database export, so a db tamper that repairs `checksums.sha256`
/// still fails here.
pub fn verify_snapshot(dir: &Path, log: &mut dyn FnMut(&str)) -> Result<SnapshotManifest> {
    reject_payload_symlinks(dir)?;
    let manifest_path = dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(SnapshotError::MissingManifest(manifest_path));
    }
    let manifest_text = fs::read_to_string(&manifest_path)?;
    let manifest: SnapshotManifest = serde_json::from_str(&manifest_text)
        .map_err(|e| SnapshotError::InvalidManifest(e.to_string()))?;
    let checksums = read_checksums(dir)?;
    for (rel, expected) in &checksums {
        let path = dir.join(rel);
        if !path.is_file() {
            return Err(SnapshotError::MissingFile(rel.clone()));
        }
        let actual = sha256_hex_of_file(&path)?;
        if actual != *expected {
            return Err(SnapshotError::ChecksumMismatch {
                path: rel.clone(),
                expected: expected.clone(),
                actual,
            });
        }
    }
    for rel in list_payload_files(dir)? {
        if !checksums.contains_key(&rel) {
            return Err(SnapshotError::UnlistedFile(rel));
        }
    }
    // Ciphertext binding: recompute the fingerprint of the packaged
    // database export(s) and compare it with every vault's manifest
    // ciphertext_hash. An empty hash is never valid.
    let db_files: Vec<String> = list_payload_files(dir)?
        .into_iter()
        .filter(|rel| rel.starts_with("payload/database/"))
        .collect();
    if db_files.is_empty() {
        return Err(SnapshotError::MissingFile("payload/database".into()));
    }
    let mut hasher = Sha256::new();
    for rel in &db_files {
        hasher.update(fs::read(dir.join(rel))?);
    }
    let db_fingerprint = format!("{:x}", hasher.finalize());
    for vault in &manifest.vaults {
        if vault.ciphertext_hash.is_empty() {
            return Err(SnapshotError::EmptyCiphertextHash);
        }
        if vault.ciphertext_hash != db_fingerprint {
            return Err(SnapshotError::CiphertextHashMismatch {
                expected: vault.ciphertext_hash.clone(),
                actual: db_fingerprint.clone(),
            });
        }
    }
    log(&format!(
        "snapshot: verified {} payload files",
        checksums.len()
    ));
    Ok(manifest)
}

/// The standby-side store: `staging/`, `current/` and `previous/` under
/// one root inside the agent's own data directory (A3). Promotion is
/// rename-only and keeps the previous verified snapshot (decision F).
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        SnapshotStore { root: root.into() }
    }
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }
    pub fn current_dir(&self) -> PathBuf {
        self.root.join("current")
    }
    pub fn previous_dir(&self) -> PathBuf {
        self.root.join("previous")
    }

    /// Copy a received snapshot into staging (replacing any partial
    /// staging left by an interrupted transfer) and verify it there.
    pub fn stage_from(
        &self,
        snapshot_dir: &Path,
        log: &mut dyn FnMut(&str),
    ) -> Result<SnapshotManifest> {
        reject_payload_symlinks(snapshot_dir)?;
        create_dir_owner_only(&self.root)?;
        let staging = self.staging_dir();
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        copy_dir_recursive(snapshot_dir, &staging)?;
        // manifest.json / checksums.sha256 are files, copied above only
        // if the source iterated them; copy_dir_recursive covers all
        // entries, so verify directly.
        let manifest = verify_snapshot(&staging, log)?;
        log("snapshot: staged and verified");
        Ok(manifest)
    }

    /// Promote the staged snapshot: verify again, rotate `current` to
    /// `previous`, then rename staging to `current`. A staging area
    /// that fails verification (for example a partial transfer) is
    /// never promoted and `current` is left untouched.
    pub fn promote(&self, log: &mut dyn FnMut(&str)) -> Result<SnapshotManifest> {
        let staging = self.staging_dir();
        if !staging.is_dir() {
            return Err(SnapshotError::MissingFile("staging".into()));
        }
        let manifest = verify_snapshot(&staging, log)?;
        let current = self.current_dir();
        let previous = self.previous_dir();
        if current.exists() {
            if previous.exists() {
                fs::remove_dir_all(&previous)?;
            }
            fs::rename(&current, &previous)?;
        }
        fs::rename(&staging, &current)?;
        log("snapshot: promoted staging to current; previous snapshot retained");
        Ok(manifest)
    }

    pub fn current_manifest(&self, log: &mut dyn FnMut(&str)) -> Result<SnapshotManifest> {
        verify_snapshot(&self.current_dir(), log)
    }
}

/// Hash helper used by tests and the transport layer to fingerprint a
/// payload without reading it into a log line.
pub fn fingerprint(data: &[u8]) -> String {
    sha256_hex_of_bytes(data)
}
