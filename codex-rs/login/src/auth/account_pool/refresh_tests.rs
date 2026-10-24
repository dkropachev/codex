use super::super::tests::pool_manager;
use super::super::tests::write_chatgpt_auth;
use super::super::*;
use crate::AuthHeaders;
use crate::ExternalAuth;
use crate::ExternalAuthFuture;
use crate::ExternalAuthRefreshContext;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

struct CountedCredential {
    auth: CodexAuth,
    refreshes: Arc<AtomicUsize>,
}

impl ExternalAuth for CountedCredential {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.auth.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(self.auth.clone())
        })
    }
}

#[tokio::test]
async fn guarded_refresh_uses_expected_pool_member_without_changing_selection() {
    let home = tempfile::tempdir().expect("home");
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(home.path(), "personal-pro", "personal@example.com");
    let pool = pool_manager(
        home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    let _ = pool.auth().await.expect("select work account");
    let member = pool
        .pools
        .values()
        .find_map(|pool| pool.member("personal-pro"))
        .expect("personal member");
    // The configured personal-pro alias is distinct from its provider account ID.
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "chatgpt-account-id",
        http::HeaderValue::from_static("provider-account-42"),
    );
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer current"),
    );
    let expected = CodexAuth::Headers(AuthHeaders::new(headers.clone()));
    let refreshes = Arc::new(AtomicUsize::new(0));
    member
        .manager
        .set_external_auth(Arc::new(CountedCredential {
            auth: expected.clone(),
            refreshes: Arc::clone(&refreshes),
        }))
        .await
        .expect("external personal credential");
    let selected = pool.status();

    pool.refresh_token_if_unchanged(&expected)
        .await
        .expect("refresh personal credential");
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(pool.status(), selected);

    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer stale"),
    );
    let stale = CodexAuth::Headers(AuthHeaders::new(headers));
    pool.refresh_token_if_unchanged(&stale)
        .await
        .expect("skip stale credential");
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(pool.status(), selected);
}
