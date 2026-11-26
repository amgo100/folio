//! `folio check --layout` as a process. The fixture is the library's
//! (`crates/folio/tests/check/layout`), whose tests check the same
//! diagnostics through the API; the binary lives in this package.

use std::fs;
use std::path::Path;

/// `folio check` shows the serious layout diagnostics, `folio check
/// --layout` all of them; `folio build` shows what plain `folio check`
/// does.
#[test]
fn folio_check_layout_shows_every_layout_diagnostic() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../folio/tests/check/layout");
    let dir = tempfile::tempdir().unwrap();
    for file in ["folio.toml", "main.fol", "styles/main.fsty"] {
        let to = dir.path().join(file);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(fixture.join(file), to).unwrap();
    }
    let codes = |args: &[&str]| -> Vec<String> {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_folio"))
            .args(args)
            .args(["--diagnostics", "json"])
            .current_dir(dir.path())
            .env_remove("FOLIO_CACHE_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        json["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["code"].as_str().map(str::to_string))
            .collect()
    };
    let plain = codes(&["check"]);
    assert_eq!(plain, ["page-overflow", "overfull-line"]);
    let all = codes(&["check", "--layout"]);
    for code in folio::layout::DETAIL_CODES {
        assert!(all.iter().any(|c| c == code), "{code}: {all:?}");
    }
    assert_eq!(all.len(), plain.len() + folio::layout::DETAIL_CODES.len());
    assert_eq!(codes(&["build"]), plain);
}
