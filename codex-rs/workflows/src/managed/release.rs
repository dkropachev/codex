use std::collections::BTreeMap;

use anyhow::bail;
use semver::Version;

/// A validated release identity advertised by a workflow Git remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWorkflowRelease {
    /// The selected stable tag, or `None` for an untagged snapshot.
    pub tag: Option<String>,
    /// The selected stable version, or `None` for an untagged snapshot.
    pub version: Option<Version>,
    /// The advertised object ID for the selected tag or `HEAD`.
    ///
    /// Its object type is not trusted until the selected ref is fetched.
    pub advertised_object_id: String,
}

#[derive(Default)]
struct TagOids {
    direct: Option<String>,
    peeled: Option<String>,
}

pub(crate) fn resolve_workflow_release(output: &str) -> anyhow::Result<ResolvedWorkflowRelease> {
    let mut head = None;
    let mut object_id_length = None;
    let mut tags = BTreeMap::<String, TagOids>::new();

    for (index, line) in output.lines().enumerate() {
        let mut fields = line.split('\t');
        let (Some(oid), Some(reference), None) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("malformed ls-remote record on line {}", index + 1);
        };
        if !matches!(oid.len(), 40 | 64)
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            || oid.bytes().all(|byte| byte == b'0')
        {
            bail!("invalid object ID on ls-remote line {}", index + 1);
        }
        if object_id_length.is_some_and(|len| len != oid.len()) {
            bail!("mixed object ID formats in ls-remote output");
        }
        object_id_length = Some(oid.len());

        if reference == "HEAD" {
            record_oid(&mut head, oid, reference)?;
            continue;
        }
        let Some(tag_ref) = reference.strip_prefix("refs/tags/") else {
            continue;
        };
        let (tag, field) = match tag_ref.strip_suffix("^{}") {
            Some(tag) => (tag, &mut tags.entry(tag.to_owned()).or_default().peeled),
            None => (
                tag_ref,
                &mut tags.entry(tag_ref.to_owned()).or_default().direct,
            ),
        };
        if !valid_tag_name(tag) {
            bail!("malformed tag ref `{reference}`");
        }
        record_oid(field, oid, reference)?;
    }

    for (tag, oids) in &tags {
        if oids.peeled.is_some() && oids.direct.is_none() {
            bail!("peeled tag `{tag}` has no direct record");
        }
    }

    let mut candidates = Vec::new();
    for (tag, oids) in tags {
        let Some(version) = stable_version(&tag) else {
            continue;
        };
        let object_id = oids
            .peeled
            .or(oids.direct)
            .expect("tag record has an object ID");
        candidates.push((tag, version, object_id));
    }
    candidates.sort_by(|left, right| {
        left.1
            .cmp_precedence(&right.1)
            .then_with(|| right.0.cmp(&left.0))
    });
    if let Some((tag, version, advertised_object_id)) = candidates.pop() {
        for (_, candidate_version, candidate_object_id) in candidates
            .iter()
            .rev()
            .take_while(|(_, candidate, _)| candidate.cmp_precedence(&version).is_eq())
        {
            if candidate_version != &version || candidate_object_id != &advertised_object_id {
                bail!("ambiguous stable tags at precedence `{version}`");
            }
        }
        return Ok(ResolvedWorkflowRelease {
            tag: Some(tag),
            version: Some(version),
            advertised_object_id,
        });
    }
    let Some(advertised_object_id) = head else {
        bail!("ls-remote returned no stable tag or HEAD");
    };
    Ok(ResolvedWorkflowRelease {
        tag: None,
        version: None,
        advertised_object_id,
    })
}

fn record_oid(slot: &mut Option<String>, oid: &str, reference: &str) -> anyhow::Result<()> {
    if let Some(previous) = slot
        && previous != oid
    {
        bail!("conflicting records for `{reference}`");
    }
    *slot = Some(oid.to_owned());
    Ok(())
}

fn stable_version(tag: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    version.pre.is_empty().then_some(version)
}

fn valid_tag_name(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.ends_with('.')
        && !tag.contains("..")
        && !tag.contains("@{")
        && tag.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with(".lock")
                && !part.bytes().any(|byte| {
                    byte <= b' '
                        || byte == 0x7f
                        || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
                })
        })
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
