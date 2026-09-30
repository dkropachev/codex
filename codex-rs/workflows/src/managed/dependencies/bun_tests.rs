use std::collections::BTreeMap;
use std::fs;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_sandboxing::LocalSandboxRuntime;
use codex_sandboxing::SandboxDirectSpawnRuntime;
use pretty_assertions::assert_eq;

use super::*;

fn absolute(path: &std::path::Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute test path")
}

fn fixture() -> (tempfile::TempDir, AbsolutePathBuf, ManagedBunEnvironment) {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = absolute(temporary.path());
    fs::create_dir_all(root.join("tools").as_path()).expect("create tools directory");
    fs::write(root.join("tools/bun").as_path(), "bun").expect("write Bun executable");
    let environment =
        materialize_bun_environment(&root.join("management")).expect("materialize Bun environment");
    (temporary, root, environment)
}

fn sources(local_packages: &[&str]) -> ValidatedDependencySources {
    ValidatedDependencySources {
        has_dependencies: true,
        local_packages: local_packages.iter().copied().map(Into::into).collect(),
    }
}

#[test]
fn materializes_private_configuration_and_isolated_directories() {
    let (_temporary, root, environment) = fixture();
    assert_eq!(environment.cache_dir, root.join("management/bun/cache"));
    assert_eq!(
        fs::read_to_string(environment.bunfig.as_path()).expect("read bunfig"),
        "env = false\ntelemetry = false\n\n[install]\nregistry = \"https://registry.npmjs.org/\"\n"
    );
    assert_eq!(
        fs::read(environment.npmrc.as_path()).expect("read npmrc"),
        Vec::<u8>::new()
    );
    for path in [
        &environment.scratch_dir,
        &environment.temp_dir,
        &environment.home_dir,
        &environment.xdg_config_dir,
        &environment.xdg_cache_dir,
        &environment.xdg_data_dir,
        &environment.xdg_state_dir,
        &environment.app_data_dir,
        &environment.local_app_data_dir,
    ] {
        assert!(
            path.as_path().is_dir(),
            "missing {}",
            path.as_path().display()
        );
    }
    assert!(
        environment
            .operation
            .path()
            .starts_with(root.join("management").as_path())
    );
}

#[test]
fn rejects_candidate_npmrc_and_environment_files_case_insensitively() {
    for name in [".npmrc", ".NPMRC", ".env", ".ENV.Local", ".env.production"] {
        let candidate = tempfile::tempdir().expect("candidate");
        fs::create_dir(candidate.path().join(name)).expect("create hostile configuration");
        assert!(
            reject_untrusted_candidate_bun_configuration(&absolute(candidate.path())).is_err(),
            "accepted {name}"
        );
    }

    let candidate = tempfile::tempdir().expect("candidate");
    for name in ["bunfig.toml", ".environment", ".env-local"] {
        fs::write(candidate.path().join(name), "").expect("write allowed candidate file");
    }
    reject_untrusted_candidate_bun_configuration(&absolute(candidate.path()))
        .expect("allow explicitly overridden or unrelated configuration");
}

#[test]
fn command_plan_keeps_private_environment_alive() {
    let (_temporary, root, environment) = fixture();
    let operation = environment.operation.path().to_path_buf();
    let candidate = root.join("candidate");
    fs::create_dir_all(candidate.as_path()).expect("create candidate");
    let plan = managed_bun_install_command_plan(
        &root.join("tools/bun"),
        &candidate,
        ManagedBunInstallLockfile::Text,
        &sources(&[]),
        &environment,
    )
    .expect("install plan");

    drop(environment);
    assert!(operation.is_dir());
    drop(plan);
    assert!(!operation.exists());
}

#[test]
fn rejects_overlapping_management_and_candidate_paths() {
    let (_temporary, root, environment) = fixture();
    for (bun, candidate) in [
        (root.join("tools/bun"), root.join("management")),
        (root.join("candidate/tool/bun"), root.join("candidate")),
    ] {
        fs::create_dir_all(candidate.as_path()).expect("create candidate");
        fs::create_dir_all(bun.as_path().parent().expect("Bun parent"))
            .expect("create Bun parent");
        fs::write(bun.as_path(), "bun").expect("write Bun executable");
        assert!(
            managed_bun_install_command_plan(
                &bun,
                &candidate,
                ManagedBunInstallLockfile::Text,
                &sources(&[]),
                &environment,
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let candidate = root.join("candidate");
        let alias = root.join("aliased-bun");
        symlink(candidate.join("tool/bun"), &alias).expect("alias candidate Bun");
        assert!(
            managed_bun_install_command_plan(
                &alias,
                &candidate,
                ManagedBunInstallLockfile::Text,
                &sources(&[]),
                &environment,
            )
            .is_err()
        );
    }
}

#[test]
fn plans_install_and_inspection_with_exact_argv_environment_and_permissions() {
    let (_temporary, root, environment) = fixture();
    let bun = root.join("tools/bun");
    let candidate = root.join("candidate");
    fs::create_dir_all(candidate.as_path()).expect("create candidate");

    let install = managed_bun_install_command_plan(
        &bun,
        &candidate,
        ManagedBunInstallLockfile::Text,
        &sources(&["vendor/local"]),
        &environment,
    )
    .expect("install plan");
    let inspection = managed_bun_binary_inspection_command_plan(&bun, &environment)
        .expect("binary inspection plan");

    let expected_args = vec![
        "--no-env-file".into(),
        "install".into(),
        "--frozen-lockfile".into(),
        "--ignore-scripts".into(),
        "--backend=copyfile".into(),
        "--registry=https://registry.npmjs.org/".into(),
        "--cache-dir".into(),
        environment.cache_dir.as_path().as_os_str().to_os_string(),
        "--config".into(),
        environment.bunfig.as_path().as_os_str().to_os_string(),
    ];
    assert_eq!(
        install.program,
        absolute(&fs::canonicalize(bun.as_path()).expect("resolve Bun executable"))
    );
    assert_eq!(install.args, expected_args);
    assert_eq!(install.cwd, candidate);
    let mut expected_env = [
        ("HOME", &environment.home_dir),
        ("USERPROFILE", &environment.home_dir),
        ("XDG_CONFIG_HOME", &environment.xdg_config_dir),
        ("XDG_CACHE_HOME", &environment.xdg_cache_dir),
        ("XDG_DATA_HOME", &environment.xdg_data_dir),
        ("XDG_STATE_HOME", &environment.xdg_state_dir),
        ("APPDATA", &environment.app_data_dir),
        ("LOCALAPPDATA", &environment.local_app_data_dir),
        ("TMPDIR", &environment.temp_dir),
        ("TEMP", &environment.temp_dir),
        ("TMP", &environment.temp_dir),
        ("BUN_INSTALL_CACHE_DIR", &environment.cache_dir),
        ("BUN_CONFIG_FILE", &environment.bunfig),
        ("NPM_CONFIG_USERCONFIG", &environment.npmrc),
        ("NPM_CONFIG_GLOBALCONFIG", &environment.npmrc),
    ]
    .map(|(name, path)| (name.into(), path.as_path().as_os_str().to_os_string()))
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    expected_env.insert("NPM_CONFIG_REGISTRY".into(), PUBLIC_REGISTRY.into());
    assert_eq!(install.env, expected_env);
    assert_eq!(
        summarized_permissions(&install.permissions),
        (
            NetworkSandboxPolicy::Enabled,
            vec![
                ("root".into(), FileSystemAccessMode::Read),
                path_access(&candidate, FileSystemAccessMode::Write),
                path_access(&environment.cache_dir, FileSystemAccessMode::Write),
                path_access(&environment.temp_dir, FileSystemAccessMode::Write),
                path_access(&environment.home_dir, FileSystemAccessMode::Write),
                path_access(&candidate.join("package.json"), FileSystemAccessMode::Read),
                path_access(&candidate.join("bun.lock"), FileSystemAccessMode::Read),
                path_access(&candidate.join(".git"), FileSystemAccessMode::Read),
                path_access(
                    &candidate.join("vendor/local/package.json"),
                    FileSystemAccessMode::Read,
                ),
                path_access(&environment.bunfig, FileSystemAccessMode::Read),
                path_access(&environment.npmrc, FileSystemAccessMode::Read),
            ],
        )
    );
    assert_eq!(inspection.program, install.program);
    let mut expected_inspection_args = expected_args;
    expected_inspection_args.insert(2, "--save-text-lockfile".into());
    expected_inspection_args.insert(3, "--lockfile-only".into());
    assert_eq!(inspection.args, expected_inspection_args);
    assert_eq!(inspection.env, install.env);
    assert_eq!(inspection.cwd, environment.scratch_dir);
    assert_eq!(
        summarized_permissions(&inspection.permissions),
        (
            NetworkSandboxPolicy::Restricted,
            vec![
                ("root".into(), FileSystemAccessMode::Read),
                path_access(&environment.scratch_dir, FileSystemAccessMode::Write),
                path_access(&environment.cache_dir, FileSystemAccessMode::Write),
                path_access(&environment.temp_dir, FileSystemAccessMode::Write),
                path_access(&environment.home_dir, FileSystemAccessMode::Write),
                path_access(
                    &environment.scratch_dir.join("package.json"),
                    FileSystemAccessMode::Read,
                ),
                path_access(&environment.bunfig, FileSystemAccessMode::Read),
                path_access(&environment.npmrc, FileSystemAccessMode::Read),
            ],
        )
    );
}

#[test]
fn required_sandbox_preparation_never_falls_back_to_unrestricted() {
    let (_temporary, root, environment) = fixture();
    let candidate = root.join("candidate");
    fs::create_dir_all(candidate.as_path()).expect("create candidate");
    let mut plan = managed_bun_install_command_plan(
        &root.join("tools/bun"),
        &candidate,
        ManagedBunInstallLockfile::Text,
        &sources(&[]),
        &environment,
    )
    .expect("install plan");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        plan.args.push(std::ffi::OsString::from_vec(vec![0xff]));
    }
    let outcome = plan
        .prepare(LocalSandboxRuntime {
            direct_spawn: SandboxDirectSpawnRuntime {
                codex_home: &root,
                windows_sandbox_wrapper_executable: None,
            },
            linux_sandbox_executable: None,
            use_legacy_landlock: false,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
        })
        .expect("classify unavailable sandbox");

    assert!(matches!(
        outcome,
        ManagedBunSandboxPreparation::Unavailable(_)
    ));
}

fn path_access(
    path: &AbsolutePathBuf,
    access: FileSystemAccessMode,
) -> (String, FileSystemAccessMode) {
    (path.to_string_lossy().into_owned(), access)
}

fn summarized_permissions(
    permissions: &PermissionProfile,
) -> (NetworkSandboxPolicy, Vec<(String, FileSystemAccessMode)>) {
    let (file_system, network) = permissions.to_runtime_permissions();
    let entries = file_system
        .entries
        .into_iter()
        .map(|entry| {
            let path = match entry.path {
                FileSystemPath::Path { path } => path
                    .to_abs_path()
                    .expect("host-local permission path")
                    .to_string_lossy()
                    .into_owned(),
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                } => "root".into(),
                path => panic!("unexpected permission path {path:?}"),
            };
            (path, entry.access)
        })
        .collect();
    (network, entries)
}
