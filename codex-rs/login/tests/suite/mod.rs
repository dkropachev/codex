// Aggregates all former standalone integration tests as modules.
#[path = "account_pool__auth_refresh.rs"]
mod account_pool_auth_refresh;
#[path = "account_pool__selection.rs"]
mod account_pool_selection;
mod auth_refresh;
mod device_code_login;
mod login_server_e2e;
mod logout;
