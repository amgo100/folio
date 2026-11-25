//! The compiler pin (SPEC §2.4, §14.3): `folio build` hands a project to
//! the version it pins from the toolchain cache, a version is installed
//! only from a release whose signatures verify, and a pin that is not
//! installed fails the build.
//!
//! The release here is a fake one, signed with a key made for the test: a
//! script that records how it was run stands in for the pinned compiler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ed25519_dalek::{Signer, SigningKey};
use folio::toolchain::{
    self, Channel, ChannelInfo, Curl, Fetch, InstallError, Toolchains, signed_message,
};
use semver::Version;

/// The release key of the fake channel.
fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

/// The signature file of a file of a channel, signed with `key`.
fn sign(key: &SigningKey, name: &str, bytes: &[u8]) -> Vec<u8> {
    let signature = key.sign(&signed_message(name, bytes)).to_bytes();
    let hex: String = signature.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}\n").into_bytes()
}

/// A fake release channel in a directory.
struct FakeChannel {
    dir: tempfile::TempDir,
}

impl FakeChannel {
    fn new(security_fixes_from: &str) -> Self {
        let channel = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        channel.publish(1, security_fixes_from);
        channel
    }

    /// Publishes a `channel.toml` with a serial, signed with the channel's
    /// key, and returns its text.
    fn publish(&self, serial: u64, security_fixes_from: &str) -> String {
        let info = format!(
            "version = 1\nserial = {serial}\nsecurity-fixes-from = \"{security_fixes_from}\"\n"
        );
        self.publish_text(&info);
        info
    }

    /// Publishes the text of a `channel.toml`, signed with the channel's
    /// key.
    fn publish_text(&self, info: &str) {
        // The file is signed under its name, not under the path of its
        // latest copy.
        let signature = sign(&key(), "channel.toml", info.as_bytes());
        self.put_signed(&Channel::info_path(), info.as_bytes(), &signature);
    }

    /// Writes a file of the channel, with its signature by `key`.
    fn put(&self, path: &str, bytes: &[u8], key: &SigningKey) {
        self.put_signed(path, bytes, &sign(key, path, bytes));
    }

    fn put_signed(&self, path: &str, bytes: &[u8], signature: &[u8]) {
        let file = self.dir.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        fs::write(format!("{}.sig", file.display()), signature).unwrap();
    }

    /// Publishes a release of a version, signed with the channel's key.
    fn release(&self, version: &str, binary: &[u8]) {
        let path = Channel::binary_path(&Version::parse(version).unwrap());
        self.put(&path, binary, &key());
    }

    fn channel(&self) -> Channel {
        Channel::new(&self.url(), &[key().verifying_key().to_bytes()]).unwrap()
    }

    fn url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }
}

/// Reads `file://` URLs without `curl`.
struct Local;

impl Fetch for Local {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let path = url.strip_prefix("file://").ok_or("not a file URL")?;
        fs::read(path).map_err(|e| e.to_string())
    }
}

fn install(
    channel: &FakeChannel,
    version: &str,
    toolchains: &Toolchains,
) -> Result<toolchain::Installed, InstallError> {
    let version = Version::parse(version).unwrap();
    toolchain::install(&version, &channel.channel(), &Local, toolchains)
}

/// The files in a directory and below it, for checking that a failed
/// install left nothing behind.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// A compiler that records the version that ran it and its arguments in
/// the file `$FOLIO_FAKE_LOG`, and exits with status 7.
const FAKE_COMPILER: &[u8] =
    b"#!/bin/sh\nprintf '%s\\n' \"$FOLIO_DISPATCHED_BY\" \"$@\" > \"$FOLIO_FAKE_LOG\"\nexit 7\n";

/// Runs `folio` in a directory, with a cache directory.
fn folio(dir: &Path, cache: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_folio"))
        .args(args)
        .current_dir(dir)
        .env("FOLIO_CACHE_DIR", cache)
        .env("FOLIO_FAKE_LOG", dir.join("fake.log"))
        .env_remove("FOLIO_DISPATCHED_BY")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A project that pins a compiler version.
fn project(pin: &str, manifest_rest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let manifest = format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"{pin}\"\n{manifest_rest}"
    );
    fs::write(dir.path().join("folio.toml"), manifest).unwrap();
    fs::write(dir.path().join("main.fol"), "= Forces\n").unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn builds_with_the_pinned_version() {
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    // Through `curl`, as `folio toolchain install` downloads.
    let version = Version::new(77, 0, 0);
    let installed = toolchain::install(&version, &channel.channel(), &Curl, &toolchains).unwrap();
    assert!(!installed.already);
    assert_eq!(installed.path, toolchains.binary(&version));
    assert_eq!(fs::read(&installed.path).unwrap(), FAKE_COMPILER);

    // A manifest for the pinned version, with an edition and a key that
    // this version does not know, goes to that version unread.
    let dir = project("77.0.0", "colour = true\n");
    let manifest = fs::read_to_string(dir.path().join("folio.toml")).unwrap();
    fs::write(
        dir.path().join("folio.toml"),
        manifest.replace("\"2027\"", "\"2031\""),
    )
    .unwrap();
    // So do flags that this version does not know or does not support.
    let output = folio(
        dir.path(),
        cache.path(),
        &["build", "--target", "pdf", "--strict", "--from-the-future"],
    );
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    let log = fs::read_to_string(dir.path().join("fake.log")).unwrap();
    assert_eq!(
        log,
        format!(
            "{}\nbuild\n--target\npdf\n--strict\n--from-the-future\n",
            folio::VERSION
        ),
        "the pinned version runs with the same arguments"
    );
    assert!(!dir.path().join("out").exists());

    // `--compiler current` builds with this version instead, with a warning.
    let dir = project("77.0.0", "");
    let output = folio(
        dir.path(),
        cache.path(),
        &["build", "--compiler", "current"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("not the pinned 77.0.0"),
        "{}",
        stderr(&output)
    );
    assert!(dir.path().join("out/main.pdf").is_file());
    assert!(!dir.path().join("fake.log").exists());

    // Installing it again downloads nothing, from the command line too.
    let output = folio(dir.path(), cache.path(), &["toolchain", "install"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("Folio 77.0.0 is already installed"));
}

#[test]
fn a_pin_that_is_not_installed_fails_the_build() {
    let cache = tempfile::tempdir().unwrap();
    let dir = project("78.0.0", "");
    let output = folio(dir.path(), cache.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains(&format!(
            "the project pins compiler 78.0.0, but this is Folio {}",
            folio::VERSION
        )),
        "{text}"
    );
    assert!(text.contains("folio.toml:4"), "{text}");
    assert!(text.contains("`folio toolchain install 78.0.0`"), "{text}");
    assert!(!dir.path().join("out").exists());
}

#[test]
fn bad_signatures_are_rejected() {
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    let rejected = |result: Result<toolchain::Installed, InstallError>, name: &str| match result {
        Err(InstallError::Signature { name: n }) => assert_eq!(n, name),
        other => panic!("{other:?}"),
    };
    let binary = Channel::binary_path(&Version::new(77, 0, 0));

    // The binary changed after it was signed.
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let file = channel.dir.path().join(&binary);
    fs::write(&file, b"#!/bin/sh\nrm -rf ~\n").unwrap();
    rejected(install(&channel, "77.0.0", &toolchains), &binary);

    // Signed with another key.
    let other = SigningKey::from_bytes(&[8; 32]);
    channel.put(&binary, FAKE_COMPILER, &other);
    rejected(install(&channel, "77.0.0", &toolchains), &binary);

    // A validly signed binary of another version.
    channel.release("76.0.0", FAKE_COMPILER);
    let signature = sign(
        &key(),
        &Channel::binary_path(&Version::new(76, 0, 0)),
        FAKE_COMPILER,
    );
    channel.put_signed(&binary, FAKE_COMPILER, &signature);
    rejected(install(&channel, "77.0.0", &toolchains), &binary);

    // Not a signature.
    channel.put_signed(&binary, FAKE_COMPILER, b"signed, honestly\n");
    rejected(install(&channel, "77.0.0", &toolchains), &binary);

    // A channel file that changed after it was signed.
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let info = channel.dir.path().join(Channel::info_path());
    fs::write(
        &info,
        "version = 1\nserial = 1\nsecurity-fixes-from = \"0.0.1\"\n",
    )
    .unwrap();
    rejected(install(&channel, "77.0.0", &toolchains), "channel.toml");

    assert_eq!(files(cache.path()), Vec::<PathBuf>::new());
    assert_eq!(toolchains.get(&Version::new(77, 0, 0)), None);

    // A missing signature is a failed download.
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    fs::remove_file(format!(
        "{}.sig",
        channel.dir.path().join(&binary).display()
    ))
    .unwrap();
    assert!(matches!(
        install(&channel, "77.0.0", &toolchains),
        Err(InstallError::Download { .. })
    ));
    assert_eq!(files(cache.path()), Vec::<PathBuf>::new());
}

#[test]
fn replaces_what_is_left_of_a_version() {
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    let left = toolchains.dir().join("77.0.0");
    fs::create_dir_all(&left).unwrap();
    fs::write(left.join("notes.txt"), "the binary was removed").unwrap();
    let installed = install(&channel, "77.0.0", &toolchains).unwrap();
    assert!(!installed.already);
    assert_eq!(fs::read(&installed.path).unwrap(), FAKE_COMPILER);
    assert!(!left.join("notes.txt").exists());
}

#[test]
fn installs_only_with_a_release_key() {
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    let keyless = Channel::new(&channel.url(), &[]).unwrap();
    let result = toolchain::install(&Version::new(77, 0, 0), &keyless, &Local, &toolchains);
    assert!(matches!(result, Err(InstallError::NoKeys)), "{result:?}");

    // The Folio project has published no release key yet.
    let dir = project("77.0.0", "");
    let output = folio(
        dir.path(),
        cache.path(),
        &["toolchain", "install", "77.0.0"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("trusts no release key"),
        "{}",
        stderr(&output)
    );
    assert_eq!(files(cache.path()), Vec::<PathBuf>::new());
}

#[test]
fn stores_what_the_channel_says_of_security_fixes() {
    let channel = FakeChannel::new("80.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    let installed = install(&channel, "77.0.0", &toolchains).unwrap();
    let info = ChannelInfo {
        serial: 1,
        security_fixes_from: Version::new(80, 0, 0),
    };
    assert_eq!(installed.info.as_ref(), Some(&info));
    let keys = [key().verifying_key()];
    assert_eq!(toolchains.channel_info(&keys), Some(info));
    // Only a release key of this compiler vouches for it.
    let other = SigningKey::from_bytes(&[8; 32]).verifying_key();
    assert_eq!(toolchains.channel_info(&[other]), None);
    assert_eq!(
        toolchain::security_fixes_from(Some(&toolchains)),
        toolchain::SECURITY_FIXES_FROM
    );
}

#[test]
fn refuses_an_older_channel_file() {
    let channel = FakeChannel::new("0.0.0");
    channel.release("77.0.0", FAKE_COMPILER);
    channel.release("78.0.0", FAKE_COMPILER);
    channel.release("79.0.0", FAKE_COMPILER);
    let cache = tempfile::tempdir().unwrap();
    let toolchains = Toolchains::new(cache.path().join("toolchains"));
    let keys = [key().verifying_key()];
    let old = fs::read(channel.dir.path().join(Channel::info_path())).unwrap();
    let old_signature = fs::read(
        channel
            .dir
            .path()
            .join(format!("{}.sig", Channel::info_path())),
    )
    .unwrap();

    // A newer channel file raises the floor.
    channel.publish(2, "78.0.0");
    install(&channel, "77.0.0", &toolchains).unwrap();
    assert_eq!(toolchains.channel_info(&keys).unwrap().serial, 2);

    // The older one, validly signed, is refused before the binary is
    // downloaded, and the stored floor stays.
    channel.put_signed(&Channel::info_path(), &old, &old_signature);
    match install(&channel, "78.0.0", &toolchains) {
        Err(
            e @ InstallError::ChannelReplayed {
                serial: 1,
                stored: 2,
            },
        ) => {
            assert!(e.to_string().contains("older than serial 2"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(toolchains.get(&Version::new(78, 0, 0)), None);
    let info = toolchains.channel_info(&keys).unwrap();
    assert_eq!(
        (info.serial, info.security_fixes_from),
        (2, Version::new(78, 0, 0))
    );

    // So is a file with the same serial and other contents.
    channel.publish(2, "0.0.0");
    assert!(matches!(
        install(&channel, "78.0.0", &toolchains),
        Err(InstallError::ChannelReplayed {
            serial: 2,
            stored: 2
        })
    ));
    assert_eq!(toolchains.get(&Version::new(78, 0, 0)), None);

    // The same file again is accepted, and so is a later one.
    channel.publish(2, "78.0.0");
    install(&channel, "78.0.0", &toolchains).unwrap();
    channel.publish(5, "79.0.0");
    install(&channel, "79.0.0", &toolchains).unwrap();
    let info = toolchains.channel_info(&keys).unwrap();
    assert_eq!(
        (info.serial, info.security_fixes_from),
        (5, Version::new(79, 0, 0))
    );
    // The file and its signature are stored together, in one file.
    let stored = toolchains.dir().join("channel.toml.signed");
    assert!(stored.is_file());
    assert!(!toolchains.dir().join("channel.toml").exists());
    assert!(!toolchains.dir().join("channel.toml.sig").exists());

    // A copy stored in a later format, by a newer compiler, gives no floor,
    // but still refuses older copies.
    let later = "version = 2\nserial = 9\nfloor = \"79.0.0\"\n";
    let signature = sign(&key(), "channel.toml", later.as_bytes());
    let mut contents = signature.clone();
    contents.extend_from_slice(later.as_bytes());
    fs::write(&stored, &contents).unwrap();
    assert_eq!(toolchains.channel_info(&keys), None);
    channel.release("80.0.0", FAKE_COMPILER);
    assert!(matches!(
        install(&channel, "80.0.0", &toolchains),
        Err(InstallError::ChannelReplayed {
            serial: 5,
            stored: 9
        })
    ));

    // A stored copy that no key of the channel signed counts for nothing:
    // another channel's, say.
    let other = SigningKey::from_bytes(&[8; 32]);
    let mut contents = sign(&other, "channel.toml", later.as_bytes());
    contents.extend_from_slice(later.as_bytes());
    fs::write(&stored, &contents).unwrap();
    install(&channel, "80.0.0", &toolchains).unwrap();
    assert_eq!(toolchains.channel_info(&keys).unwrap().serial, 5);

    // Serials may use digit separators.
    channel.publish(1_000, "79.0.0");
    channel.release("81.0.0", FAKE_COMPILER);
    install(&channel, "81.0.0", &toolchains).unwrap();
    assert_eq!(toolchains.channel_info(&keys).unwrap().serial, 1_000);
}

#[test]
fn reads_channel_files() {
    let info =
        ChannelInfo::parse("version = 1\nserial = 3\nsecurity-fixes-from = \"1.4.0\"\nnews = 3\n")
            .unwrap();
    assert_eq!(info.serial, 3);
    assert_eq!(info.security_fixes_from, Version::new(1, 4, 0));
    let floor = "security-fixes-from = \"1.4.0\"\n";
    assert!(ChannelInfo::parse(&format!("version = 2\nserial = 3\n{floor}")).is_err());
    assert!(ChannelInfo::parse("version = 1\nserial = 3\n").is_err());
    assert!(
        ChannelInfo::parse("version = 1\nserial = 3\nsecurity-fixes-from = \"1.4\"\n").is_err()
    );
    // The serial is required, and is a whole number that is not negative.
    assert!(ChannelInfo::parse(&format!("version = 1\n{floor}")).is_err());
    assert!(ChannelInfo::parse(&format!("version = 1\nserial = -1\n{floor}")).is_err());
    assert!(ChannelInfo::parse(&format!("version = 1\nserial = 1.5\n{floor}")).is_err());
    assert!(ChannelInfo::parse(&format!("version = 1\nserial = \"3\"\n{floor}")).is_err());
    let info = ChannelInfo::parse(&format!("version = 1\nserial = 1_200\n{floor}"));
    assert_eq!(info.unwrap().serial, 1200);
    assert!(ChannelInfo::parse("version = ").is_err());
}
