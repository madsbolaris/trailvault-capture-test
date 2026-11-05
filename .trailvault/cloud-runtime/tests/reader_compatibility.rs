use anyhow::{Result, ensure};
use serde_json::json;
use trailvault::relationships::{self, Target};

#[test]
fn current_reader_recognizes_contextual_pr_page_links_without_promoting_proof() -> Result<()> {
    let target = Target::parse("https://github.com/owner/repo/pull/39")?;
    let compact = [
        json!({"type":"header","version":1}),
        json!({"type":"tool.execution_complete","data":{
            "toolCallId":"page","result":{"content":"/owner/repo/pull/39"}
        }}),
    ]
    .into_iter()
    .map(|row| row.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let links = relationships::collect(&compact, Some("owner/repo"), None)?;
    ensure!(links.relationships.len() == 1);
    let link = &links.relationships[0];
    ensure!(link.target == target && !link.retrieval_verified && !link.contribution_verified);
    ensure!(
        relationships::mentions(
            "https://evil.example/owner/repo/pull/39",
            Some("owner/repo")
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn retained_empty_activity_rows_are_compatible_but_assertions_still_require_evidence() -> Result<()>
{
    let actual = relationships::ActivityCollection {
        version: 1,
        relationships: vec![relationships::SessionActivity {
            target: Target::parse("https://github.com/owner/repo/pull/36")?,
            activities: vec![],
            evidence: vec![],
        }],
    };
    let mut stored = serde_json::to_value(&actual)?;
    stored["relationships"].as_array_mut().unwrap().push(json!({
        "target":{"host":"github.com","repository":"owner/repo","number":39},
        "activities":[],"evidence":[]
    }));
    relationships::validate_activities(&stored, &actual)?;
    for (activities, evidence) in [
        (json!(["reviewed"]), json!([])),
        (
            json!([]),
            json!([{"activity":"read","kind":"tool_retrieval","record":1}]),
        ),
    ] {
        let mut forged = stored.clone();
        forged["relationships"][1]["activities"] = activities;
        forged["relationships"][1]["evidence"] = evidence;
        ensure!(relationships::validate_activities(&forged, &actual).is_err());
    }
    stored["version"] = json!(2);
    ensure!(relationships::validate_activities(&stored, &actual).is_err());
    Ok(())
}
