use super::*;
use codex_file_system::CopyOptions;
use codex_file_system::CreateDirectoryOptions;
use codex_file_system::ExecutorFileSystemFuture;
use codex_file_system::FileMetadata;
use codex_file_system::FileSystemReadStream;
use codex_file_system::FileSystemSandboxContext;
use codex_file_system::ReadDirectoryEntry;
use codex_file_system::RemoveOptions;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

struct TestFileSystem;

impl ExecutorFileSystem for TestFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        Box::pin(async move {
            let path = path.to_abs_path()?;
            let canonicalized = path.canonicalize()?;
            Ok(PathUri::from_abs_path(&canonicalized))
        })
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let path = path.to_abs_path()?;
            tokio::fs::read(path.as_path()).await
        })
    }

    fn read_file_stream<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "test filesystem does not support streaming reads",
            ))
        })
    }

    fn write_file<'a>(
        &'a self,
        _path: &'a PathUri,
        _contents: Vec<u8>,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn create_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _create_directory_options: CreateDirectoryOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn get_metadata<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn read_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn remove<'a>(
        &'a self,
        _path: &'a PathUri,
        _remove_options: RemoveOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn copy<'a>(
        &'a self,
        _source_path: &'a PathUri,
        _destination_path: &'a PathUri,
        _copy_options: CopyOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }
}

#[tokio::test]
async fn ignored_user_config_keeps_only_account_pool_auth_routing() {
    let tmp = tempdir().expect("tempdir");
    let user_file = AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, tmp.path());
    std::fs::write(
        user_file.as_path(),
        r#"
model = "ignored-model"
cli_auth_credentials_store = "keyring"

[features]
secret_auth_storage = true
web_search_request = true

[account_pool]
enabled = true
default_pool = "primary"

[account_pool.pools.primary]
provider = "openai"
policy = "drain"
accounts = ["member"]

[mcp_servers.ignored]
command = "ignored-command"
"#,
    )
    .expect("write user config");

    let actual = load_user_config_layer(
        &TestFileSystem,
        &user_file,
        /*profile*/ None,
        /*ignore_user_config*/ true,
        /*strict_config*/ true,
    )
    .await
    .expect("load ignored user config");
    let expected_config = toml::from_str(
        r#"
cli_auth_credentials_store = "keyring"

[features]
secret_auth_storage = true

[account_pool]
enabled = true
default_pool = "primary"

[account_pool.pools.primary]
provider = "openai"
policy = "drain"
accounts = ["member"]
"#,
    )
    .expect("parse expected config");

    assert_eq!(
        actual,
        ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: user_file,
                profile: None,
            },
            expected_config,
        )
    );
}

#[tokio::test]
async fn ignored_profile_config_preserves_partial_account_pool_overlay() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");
    let base_config = r#"
[account_pool]
enabled = true
default_pool = "primary"

[account_pool.pools.primary]
provider = "openai"
policy = "drain"
accounts = ["primary-member"]

[account_pool.pools.secondary]
provider = "openai"
policy = "drain"
accounts = ["secondary-member"]
"#;
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), base_config).expect("write base user config");
    std::fs::write(
        &selected_config,
        r#"
[account_pool]
default_pool = "secondary"
"#,
    )
    .expect("write selected user config");
    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(
        AbsolutePathBuf::from_absolute_path(&selected_config).expect("absolute selected config"),
    );
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));
    overrides.ignore_user_config = true;

    let layers = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect("load ignored profile config");
    let config: ConfigToml = layers
        .effective_config()
        .try_into()
        .expect("deserialize merged config");
    let mut expected: ConfigToml = toml::from_str(base_config).expect("parse base config");
    expected
        .account_pool
        .as_mut()
        .expect("base account pool")
        .default_pool = Some("secondary".to_string());

    assert_eq!(config, expected);
}

#[tokio::test]
async fn ignored_user_config_drops_invalid_account_pool_auth_routing() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
model = "ignored-model"

[account_pool]
enabled = true
"#,
    )
    .expect("write invalid account pool");
    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.ignore_user_config = true;

    let layers = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect("ignore invalid account pool");
    let user_file = AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, tmp.path());

    assert_eq!(
        layers.get_active_user_layer().expect("active user layer"),
        &ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: user_file,
                profile: None,
            },
            TomlValue::Table(toml::map::Map::new()),
        )
    );
}

#[tokio::test]
async fn profile_v2_rejects_matching_legacy_profile_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
model = "gpt-main"

[profiles.work]
model = "gpt-work"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    let err = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect_err("profile-v2 should reject a matching legacy profile in base user config");

    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a matching legacy profile should be a hard config error"
    );
    let message = err.to_string();
    assert!(
        message.contains("--profile `work` cannot be used"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("config.toml"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("[profiles.work]"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("https://developers.openai.com/codex/config-advanced#profiles"),
        "unexpected error message: {message}"
    );
}

#[tokio::test]
async fn profile_v2_rejects_matching_legacy_profile_selector_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
profile = "work"
model = "gpt-main"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    let err = load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect_err("profile-v2 should reject a matching legacy profile selector");

    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a matching legacy profile selector should be a hard config error"
    );
    let message = err.to_string();
    assert!(
        message.contains("--profile `work` cannot be used"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("profile = \"work\""),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("work.config.toml"),
        "unexpected error message: {message}"
    );
}

#[tokio::test]
async fn profile_v2_allows_unrelated_legacy_profiles_in_base_user_config() {
    let tmp = tempdir().expect("tempdir");
    let selected_config = tmp.path().join("work.config.toml");

    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
model = "gpt-main"

[profiles.dev]
model = "gpt-dev"
"#,
    )
    .expect("write default user config");
    std::fs::write(&selected_config, r#"model = "gpt-work-v2""#)
        .expect("write selected user config");

    let mut overrides = LoaderOverrides::without_managed_config_for_tests();
    overrides.user_config_path = Some(AbsolutePathBuf::resolve_path_against_base(
        "work.config.toml",
        tmp.path(),
    ));
    overrides.user_config_profile = Some("work".parse().expect("profile-v2 name"));

    load_config_layers_state(
        &TestFileSystem,
        tmp.path(),
        /*cwd*/ None,
        &[],
        overrides,
        &crate::NoopThreadConfigLoader,
    )
    .await
    .expect("profile-v2 should allow unrelated legacy profiles in base user config");
}
