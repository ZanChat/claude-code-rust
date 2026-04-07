use std::path::PathBuf;

use pretty_assertions::assert_eq;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn live_parity_manifest_covers_workspace_modules() {
    let workspace_file = workspace_root().join("Claude-Code-main-run.code-workspace");
    let audit = crate::audit_module_parity_with_workspace_file(&workspace_file).unwrap();

    assert_eq!(audit.duplicate_manifest_entries, Vec::<String>::new());
    assert_eq!(audit.unclassified_ts_modules, Vec::<String>::new());
    assert_eq!(audit.stale_manifest_entries, Vec::<String>::new());
    assert_eq!(audit.unmapped_rust_crates, Vec::<String>::new());
}

#[test]
fn parity_manifest_notes_avoid_placeholder_copy() {
    for entry in crate::module_parity_manifest() {
        let lowered = entry.note.to_ascii_lowercase();
        for needle in crate::parity_placeholder_terms() {
            assert!(
                !lowered.contains(needle),
                "unexpected placeholder copy '{needle}' in parity note for {}: {}",
                entry.ts_module,
                entry.note
            );
        }
    }
}

#[test]
fn doctor_command_reports_live_parity_audit() {
    let output = crate::render_doctor_command(&workspace_root()).unwrap();

    assert!(output.contains("Rewrite parity audit"));
    assert!(output.contains("Unclassified TS modules: 0"));
    assert!(output.contains("Stale manifest entries: 0"));
    assert!(output.contains("Unmapped Rust crates: 0"));
    assert!(output.contains("voice -> split"));
    assert!(output.contains("ssh -> removed"));

    let lowered = output.to_ascii_lowercase();
    for needle in crate::parity_placeholder_terms() {
        assert!(
            !lowered.contains(needle),
            "unexpected placeholder copy '{needle}' in /doctor output: {output}"
        );
    }
}
