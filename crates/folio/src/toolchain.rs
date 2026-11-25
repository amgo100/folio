//! Compiler versions: the local toolchain cache that holds the versions
//! projects pin, and installing a version from the Folio release channel
//! (SPEC §2.4, §14.3).
//!
//! `folio.toml` pins the compiler version that fixes a project's output. A
//! host that is not that version runs the pinned one from the toolchain
//! cache ([`Toolchains`]), and fails the build when it is not installed.
//! [`install`] is the only way a version enters the cache: it downloads the
//! release from the channel and installs it only when its signature, made
//! with a key compiled into this binary, verifies. Building never installs
//! anything.
//!
//! # The release channel
//!
//! A channel is a base URL with this layout, which GitHub releases give
//! (SPEC §14.3):
//!
//! - `download/v{version}/{asset}`: the compiler binary for a platform,
//!   named by [`asset_name`], such as `folio-x86_64-linux`;
//! - `latest/download/channel.toml`: what the project says about its
//!   releases now ([`ChannelInfo`]);
//! - each file's signature beside it, with `.sig` added to its name.
//!
//! A signature is the hexadecimal Ed25519 signature, by a release key, of
//! [`signed_message`] for the file: its path in the channel and the SHA-256
//! hash of its bytes. Signing the path binds a binary to its version and
//! platform, so a signed binary of one version cannot pass for another.

use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};

/// The Folio project's release channel.
pub const CHANNEL_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases");

/// The public keys that sign releases on [`CHANNEL_URL`]. The project has
/// published no release yet, so there is no key, and [`install`] refuses
/// the official channel until the first release adds one (SPEC §16.1).
pub const RELEASE_KEYS: &[[u8; 32]] = &[];

/// The oldest compiler version that received security fixes when this
/// version was released. A newer `channel.toml` that [`install`] has stored
/// may raise it ([`security_fixes_from`]).
pub const SECURITY_FIXES_FROM: Version = Version::new(0, 0, 0);

/// The local toolchain cache: one directory per installed version, named
/// by the version, holding its `folio` binary, and the last `channel.toml`
/// that [`install`] verified, with its signature.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Toolchains {
    dir: PathBuf,
}

impl Toolchains {
    /// The cache in a directory, which need not exist yet.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The default directory: `toolchains` in Folio's cache directory
    /// ([`folio_fonts::cache_root`]). `None` when there is no such
    /// directory.
    pub fn default_dir() -> Option<PathBuf> {
        folio_fonts::cache_root().map(|root| root.join("toolchains"))
    }

    /// The cache's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path that the binary of a version is installed at.
    pub fn binary(&self, version: &Version) -> PathBuf {
        self.dir
            .join(version.to_string())
            .join(format!("folio{}", std::env::consts::EXE_SUFFIX))
    }

    /// The binary of a version, or `None` when the version is not
    /// installed.
    pub fn get(&self, version: &Version) -> Option<PathBuf> {
        let path = self.binary(version);
        path.is_file().then_some(path)
    }

    /// The last `channel.toml` that [`install`] stored, if its signature
    /// verifies with one of `keys`.
    pub fn channel_info(&self, keys: &[VerifyingKey]) -> Option<ChannelInfo> {
        let bytes = fs::read(self.dir.join(CHANNEL_FILE)).ok()?;
        let signature = fs::read(self.dir.join(format!("{CHANNEL_FILE}.sig"))).ok()?;
        verify(keys, CHANNEL_FILE, &bytes, &signature).ok()?;
        ChannelInfo::parse(std::str::from_utf8(&bytes).ok()?).ok()
    }
}

/// The oldest compiler version that still receives security fixes, as far
/// as this compiler knows: [`SECURITY_FIXES_FROM`], or the newer version
/// that the official channel's `channel.toml` in `toolchains` names. A
/// project that pins an older version gets a warning (SPEC §14.3).
pub fn security_fixes_from(toolchains: Option<&Toolchains>) -> Version {
    let keys = Channel::official().keys;
    toolchains
        .and_then(|t| t.channel_info(&keys))
        .map(|info| info.security_fixes_from)
        .filter(|v| *v > SECURITY_FIXES_FROM)
        .unwrap_or(SECURITY_FIXES_FROM)
}

/// The name of the channel's file about its releases.
const CHANNEL_FILE: &str = "channel.toml";

/// What a channel says about its releases in `channel.toml`:
///
/// ```toml
/// version = 1                     # the file's format
/// security-fixes-from = "1.0.0"   # the oldest version still fixed
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ChannelInfo {
    /// The oldest compiler version that still receives security fixes.
    pub security_fixes_from: Version,
}

impl ChannelInfo {
    /// The format of `channel.toml` that this compiler reads.
    pub const FORMAT: i64 = 1;

    /// Parses the text of a `channel.toml`. Keys this compiler does not
    /// know are ignored, so that a later format can add some.
    pub fn parse(text: &str) -> Result<Self, String> {
        use toml::de::{DeTable, DeValue};

        let root = DeTable::parse(text).map_err(|e| e.message().trim_end().to_owned())?;
        let root = root.get_ref();
        let get = |key: &str| root.get(key).map(|v| v.get_ref());
        let format = match get("version") {
            Some(DeValue::Integer(i)) => i64::from_str_radix(i.as_str(), i.radix()).ok(),
            _ => None,
        };
        if format != Some(Self::FORMAT) {
            return Err(format!("expected `version = {}`", Self::FORMAT));
        }
        let floor = match get("security-fixes-from") {
            Some(DeValue::String(text)) => Version::parse(text).ok(),
            _ => None,
        };
        let floor = floor.ok_or("expected `security-fixes-from`, a version")?;
        Ok(Self {
            security_fixes_from: floor,
        })
    }
}

/// A release channel: where releases are downloaded from, and the keys
/// their signatures must verify with.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Channel {
    url: String,
    keys: Vec<VerifyingKey>,
}

impl Channel {
    /// The Folio project's channel, [`CHANNEL_URL`] with [`RELEASE_KEYS`].
    /// It is the only channel that `folio toolchain install` uses.
    pub fn official() -> Self {
        Self::new(CHANNEL_URL, RELEASE_KEYS).expect("the release keys are valid")
    }

    /// A channel at a base URL, whose releases are signed with one of
    /// `keys`. Fails when a key is not a valid Ed25519 public key.
    pub fn new(url: &str, keys: &[[u8; 32]]) -> Result<Self, InstallError> {
        let keys = keys
            .iter()
            .map(VerifyingKey::from_bytes)
            .collect::<Result<_, _>>()
            .map_err(|_| InstallError::InvalidKey)?;
        Ok(Self {
            url: url.trim_end_matches('/').to_owned(),
            keys,
        })
    }

    /// The channel's base URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The path in the channel of a version's binary for this platform.
    pub fn binary_path(version: &Version) -> String {
        format!("download/v{version}/{}", asset_name())
    }

    /// The path in the channel of the current `channel.toml`.
    pub fn info_path() -> String {
        format!("latest/download/{CHANNEL_FILE}")
    }

    /// Downloads a file of the channel and its signature, and checks the
    /// signature. `name` is what the signature signs. Returns the file's
    /// bytes and the signature.
    fn fetch(
        &self,
        fetch: &dyn Fetch,
        path: &str,
        name: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), InstallError> {
        let get = |path: &str| {
            let url = format!("{}/{path}", self.url);
            fetch
                .fetch(&url)
                .map_err(|message| InstallError::Download { url, message })
        };
        let bytes = get(path)?;
        let signature = get(&format!("{path}.sig"))?;
        verify(&self.keys, name, &bytes, &signature)?;
        Ok((bytes, signature))
    }
}

/// The name of the compiler binary for this platform in a release:
/// `folio-{arch}-{os}`, with `.exe` on Windows.
pub fn asset_name() -> String {
    use std::env::consts::{ARCH, EXE_SUFFIX, OS};
    format!("folio-{ARCH}-{OS}{EXE_SUFFIX}")
}

/// What a release key signs for a file of the channel: its path in the
/// channel and the SHA-256 hash of its bytes.
pub fn signed_message(name: &str, bytes: &[u8]) -> Vec<u8> {
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("folio release signature 1\n{name}\nsha256:{hash}\n").into_bytes()
}

/// Checks that `signature`, the text of a `.sig` file, signs the file
/// `name` with these bytes with one of `keys`.
pub fn verify(
    keys: &[VerifyingKey],
    name: &str,
    bytes: &[u8],
    signature: &[u8],
) -> Result<(), InstallError> {
    let invalid = || InstallError::Signature {
        name: name.to_owned(),
    };
    let text = std::str::from_utf8(signature)
        .map_err(|_| invalid())?
        .trim();
    let mut raw = [0; Signature::BYTE_SIZE];
    if text.len() != 2 * raw.len() || !text.is_ascii() {
        return Err(invalid());
    }
    for (byte, pair) in raw.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    let signature = Signature::from_bytes(&raw);
    let message = signed_message(name, bytes);
    keys.iter()
        .any(|key| key.verify_strict(&message, &signature).is_ok())
        .then_some(())
        .ok_or_else(invalid)
}

/// Downloads files.
pub trait Fetch {
    /// The bytes at a URL, or why they cannot be had.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Downloads with the `curl` command, which Windows, macOS and Linux
/// systems ship, over HTTPS only, or from `file://` URLs for a local
/// channel. It follows redirects only to the same scheme, and uses the
/// proxy that the environment names.
#[derive(Debug, Copy, Clone, Default)]
pub struct Curl;

impl Fetch for Curl {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let scheme = match url.split_once("://") {
            Some(("https", _)) => "=https",
            Some(("file", _)) => "=file",
            _ => return Err("Folio downloads only over HTTPS".into()),
        };
        let output = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--proto", scheme, "--proto-redir", scheme])
            .args(["--retry", "3", "--connect-timeout", "30"])
            .args(["--max-filesize", "1073741824"])
            .arg(url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => {
                    "Folio downloads with `curl`, which is not installed".into()
                }
                _ => format!("cannot run `curl`: {e}"),
            })?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let message = String::from_utf8_lossy(&output.stderr);
            let message = message.trim().trim_start_matches("curl: ");
            Err(match message {
                "" => format!("`curl` failed ({})", output.status),
                message => message.to_owned(),
            })
        }
    }
}

/// What [`install`] did.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Installed {
    /// The installed binary.
    pub path: PathBuf,
    /// Whether the version was installed already, so nothing was
    /// downloaded.
    pub already: bool,
    /// The channel's `channel.toml`, which was downloaded with the release.
    pub info: Option<ChannelInfo>,
}

/// Installs a compiler version from a channel into the toolchain cache.
///
/// Downloads the channel's `channel.toml` and the version's binary for
/// this platform, with their signatures, and installs only when both
/// signatures verify with one of the channel's keys. The binary appears in
/// the cache whole or not at all. A version that is installed already is
/// left as it is.
pub fn install(
    version: &Version,
    channel: &Channel,
    fetch: &dyn Fetch,
    toolchains: &Toolchains,
) -> Result<Installed, InstallError> {
    if let Some(path) = toolchains.get(version) {
        return Ok(Installed {
            path,
            already: true,
            info: None,
        });
    }
    if channel.keys.is_empty() {
        return Err(InstallError::NoKeys);
    }
    let (info_bytes, info_signature) = channel.fetch(fetch, &Channel::info_path(), CHANNEL_FILE)?;
    let info = std::str::from_utf8(&info_bytes)
        .map_err(|e| e.to_string())
        .and_then(ChannelInfo::parse)
        .map_err(InstallError::ChannelInfo)?;
    let binary_path = Channel::binary_path(version);
    let (binary, _) = channel.fetch(fetch, &binary_path, &binary_path)?;

    let dir = toolchains.dir();
    let io = |path: &Path| {
        let path = path.to_owned();
        move |error| InstallError::Io { path, error }
    };
    fs::create_dir_all(dir).map_err(io(dir))?;
    let staging = dir.join(format!(".install-{version}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let path = toolchains.binary(version);
    let file_name = path.file_name().expect("the binary has a file name");
    let result = (|| {
        fs::create_dir(&staging)?;
        let file = staging.join(file_name);
        fs::write(&file, &binary)?;
        make_executable(&file)?;
        fs::rename(
            &staging,
            path.parent().expect("the binary is in a directory"),
        )
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        // Another install of the same version finished first.
        if let Some(path) = toolchains.get(version) {
            return Ok(Installed {
                path,
                already: true,
                info: Some(info),
            });
        }
        return Err(io(&path)(error));
    }

    // The channel's word on security fixes, for later builds, which verify
    // it again when they read it. The version is installed whether or not
    // this is stored: without it, builds know only what they were released
    // with.
    for (name, bytes) in [
        (CHANNEL_FILE.to_owned(), &info_bytes[..]),
        (format!("{CHANNEL_FILE}.sig"), &info_signature[..]),
    ] {
        let temporary = dir.join(format!(".{name}.{}", std::process::id()));
        let stored =
            fs::write(&temporary, bytes).and_then(|()| fs::rename(&temporary, dir.join(&name)));
        if stored.is_err() {
            let _ = fs::remove_file(&temporary);
        }
    }
    Ok(Installed {
        path,
        already: false,
        info: Some(info),
    })
}

/// Lets the owner run a file.
#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// Lets the owner run a file.
#[cfg(not(unix))]
fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Why a version could not be installed.
#[derive(Debug)]
#[non_exhaustive]
pub enum InstallError {
    /// The channel has no key to verify signatures with.
    NoKeys,
    /// A channel key is not a valid Ed25519 public key.
    InvalidKey,
    /// A file could not be downloaded.
    Download {
        /// Its URL.
        url: String,
        /// Why.
        message: String,
    },
    /// A file's signature does not verify with any of the channel's keys,
    /// or is not a signature.
    Signature {
        /// The file's path in the channel.
        name: String,
    },
    /// The channel's `channel.toml` cannot be read.
    ChannelInfo(String),
    /// The toolchain cache cannot be written.
    Io {
        /// The path written.
        path: PathBuf,
        /// What went wrong.
        error: io::Error,
    },
}

impl Display for InstallError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKeys => f.write_str(
                "this Folio trusts no release key, so it cannot verify a release to install",
            ),
            Self::InvalidKey => f.write_str("a release key is not an Ed25519 public key"),
            Self::Download { url, message } => write!(f, "cannot download {url}: {message}"),
            Self::Signature { name } => write!(
                f,
                "the signature of `{name}` does not verify with a Folio release key; nothing was \
                 installed"
            ),
            Self::ChannelInfo(message) => {
                write!(
                    f,
                    "the release channel's {CHANNEL_FILE} is invalid: {message}"
                )
            }
            Self::Io { path, error } => write!(f, "cannot write `{}`: {error}", path.display()),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}
