// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_paths::{
    StorageConfig, read_storage_config, validate_storage_config, write_storage_config,
};
use std::fs;

#[test]
fn portable_policy_has_one_movable_directory_and_reset_preserves_data() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    fs::create_dir(&root).unwrap();
    let policy = StorageConfig {
        portable: true,
        data_dir: None,
    };
    let dir = validate_storage_config(&root, &policy).unwrap();
    assert!(dir.ends_with(".aden/data"));
    write_storage_config(&root, Some(&policy)).unwrap();
    assert_eq!(read_storage_config(&root).unwrap(), Some(policy));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("graph.snapshot"), "retained").unwrap();
    fs::write(root.join(".aden/constitution.adoc"), "intent").unwrap();
    let moved = fixture.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    assert_eq!(
        validate_storage_config(&moved, &read_storage_config(&moved).unwrap().unwrap()).unwrap(),
        moved.canonicalize().unwrap().join(".aden/data")
    );
    write_storage_config(&moved, None).unwrap();
    assert!(moved.join(".aden/data/graph.snapshot").exists());
    assert!(moved.join(".aden/constitution.adoc").exists());
    assert!(read_storage_config(&moved).unwrap().is_none());
}

#[test]
fn invalid_policy_never_creates_config_or_data() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    fs::create_dir(&root).unwrap();
    for policy in [
        StorageConfig {
            portable: false,
            data_dir: None,
        },
        StorageConfig {
            portable: true,
            data_dir: Some(fixture.path().join("outside")),
        },
        StorageConfig {
            portable: false,
            data_dir: Some("relative".into()),
        },
        StorageConfig {
            portable: false,
            data_dir: Some(root.join("cache")),
        },
        StorageConfig {
            portable: false,
            data_dir: Some(fixture.path().join("outside/../escape")),
        },
    ] {
        assert!(write_storage_config(&root, Some(&policy)).is_err());
        assert!(!root.join(".aden").exists());
    }
}

#[test]
fn malformed_unknown_and_conflicting_config_fail_closed_and_reset_recovers() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir(root.join(".aden")).unwrap();
    for body in [
        "invalid [",
        "portable = true\nunknown = true",
        "portable = false",
        "portable = true\ndata_dir = '/tmp/data'",
    ] {
        fs::write(root.join(".aden/storage.toml"), body).unwrap();
        assert!(read_storage_config(root).is_err());
    }
    write_storage_config(root, None).unwrap();
    assert!(read_storage_config(root).unwrap().is_none());
}

#[test]
fn external_location_is_keyed_and_rejects_a_different_recorded_owner() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    fs::create_dir(&root).unwrap();
    let base = fixture.path().join("external");
    let policy = StorageConfig {
        portable: false,
        data_dir: Some(base),
    };
    let dir = validate_storage_config(&root, &policy).unwrap();
    assert_eq!(
        dir.file_name().unwrap(),
        aden_paths::project_key(&root).as_str()
    );
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("meta.json"),
        r#"{"root":"different-project","key":"incorrect"}"#,
    )
    .unwrap();
    assert!(validate_storage_config(&root, &policy).is_err());
}

#[test]
fn environment_precedence_and_whole_project_layout_are_preserved() {
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }
    let _restore = EnvRestore(
        ["ADEN_DATA_DIR", "ADEN_STORE"]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
    );
    unsafe {
        std::env::remove_var("ADEN_DATA_DIR");
        std::env::remove_var("ADEN_STORE");
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    fs::create_dir(&root).unwrap();
    write_storage_config(
        &root,
        Some(&StorageConfig {
            portable: true,
            data_dir: None,
        }),
    )
    .unwrap();
    let portable = aden_paths::try_project_dir(&root).unwrap();
    assert_eq!(aden_paths::store_dir(&root), portable.join("store"));
    assert_eq!(aden_paths::cache_dir(&root), portable.join("cache"));
    assert_eq!(
        aden_paths::gen_cache_file(&root),
        portable.join("gen-cache.json")
    );
    assert_eq!(
        aden_paths::graph_snapshot_file(&root),
        portable.join("graph.snapshot")
    );
    aden_paths::write_meta(&root).unwrap();
    assert!(portable.join("meta.json").exists());
    let override_base = fixture.path().join("override");
    unsafe {
        std::env::set_var("ADEN_DATA_DIR", &override_base);
    }
    assert_eq!(
        aden_paths::try_project_dir(&root).unwrap(),
        override_base
            .join("projects")
            .join(aden_paths::project_key(&root))
    );
    let pinned_store = fixture.path().join("pinned-store");
    unsafe {
        std::env::set_var("ADEN_STORE", &pinned_store);
    }
    assert_eq!(aden_paths::store_dir(&root), pinned_store);
    assert_eq!(
        aden_paths::cache_dir(&root),
        override_base
            .join("projects")
            .join(aden_paths::project_key(&root))
            .join("cache")
    );
    fs::write(root.join(".aden/storage.toml"), "invalid [").unwrap();
    assert!(aden_paths::try_project_dir(&root).is_err());
    assert!(aden_paths::guard_creatable_root(&root, true).is_err());
}
