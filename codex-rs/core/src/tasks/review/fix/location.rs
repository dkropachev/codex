use codex_protocol::protocol::ReviewExternalReference;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_utils_path_uri::PathUri;

use crate::session::turn_context::TurnContext;

pub(crate) async fn sanitize_fix_locations(
    ctx: &TurnContext,
    checkout_root: &PathUri,
    output: &mut ReviewOutputEvent,
) {
    let Some(environment) = ctx.environments.primary() else {
        output.unverified_findings.append(&mut output.findings);
        return;
    };
    let filesystem = environment.environment.get_filesystem();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(/*secs*/ 15);
    let canonical_root = tokio::time::timeout_at(
        deadline,
        filesystem.canonicalize(checkout_root, /*sandbox*/ None),
    )
    .await
    .ok()
    .and_then(Result::ok);
    let Some(canonical_root) = canonical_root else {
        output.unverified_findings.append(&mut output.findings);
        output
            .references
            .push(codex_protocol::protocol::ReviewReference {
                reference: "review finding locations".to_string(),
                explanation: "Finding paths could not be validated against the checkout."
                    .to_string(),
            });
        return;
    };

    let mut eligible = Vec::new();
    for finding in output.findings.drain(..) {
        let display_path = finding
            .code_location
            .absolute_file_path
            .display()
            .to_string();
        let requested =
            PathUri::parse(&display_path).or_else(|_| checkout_root.join(&display_path));
        let location_is_safe = match requested {
            Ok(requested) => {
                let mut resolved = None;
                for candidate in requested.ancestors().take(64) {
                    if let Some(canonical) = tokio::time::timeout_at(
                        deadline,
                        filesystem.canonicalize(&candidate, /*sandbox*/ None),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    {
                        resolved = Some(canonical);
                        break;
                    }
                }
                resolved.is_some_and(|path| path.starts_with(&canonical_root))
            }
            Err(_) => false,
        };
        if location_is_safe {
            eligible.push(finding);
        } else {
            output.external_references.push(ReviewExternalReference {
                reference: display_path,
                explanation: "Finding path was outside the checkout or could not be resolved and is report-only."
                    .to_string(),
            });
            output.unverified_findings.push(finding);
        }
    }
    output.findings = eligible;
}
