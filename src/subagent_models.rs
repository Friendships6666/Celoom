//! User-defined subagent route choices, independent of vendor names and secrets.
use crate::{
    models::{AppConfig, Selection, Session},
    network::Snapshot,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub fn resolve(
    config: &AppConfig,
    parent: &Session,
    choice: Option<&str>,
) -> Result<(Selection, Option<String>, Option<String>)> {
    if choice.is_none() || choice == Some("inherit") {
        return Ok((
            parent.selection.clone(),
            parent.reasoning_effort.clone(),
            None,
        ));
    }
    let choice = choice.unwrap();
    let entry=config.subagent_models.iter().find(|entry|entry.id==choice).context("Unknown subagent model choice; use subagent action=models to list the user's allowed choices")?;
    let mut child = parent.clone();
    child.selection = entry.selection.clone();
    child.reasoning_effort = entry.reasoning_effort.clone();
    Snapshot::for_session(config, &child)
        .context("Configured subagent choice is unavailable; update the model pool settings")?;
    Ok((
        child.selection,
        child.reasoning_effort,
        Some(entry.id.clone()),
    ))
}
pub fn catalog(config: &AppConfig) -> Result<Value> {
    let mut entries = Vec::new();
    for entry in &config.subagent_models {
        let site = config
            .sites
            .iter()
            .find(|s| s.id == entry.selection.site_id)
            .context("Subagent pool references a removed site")?;
        let key = site
            .keys
            .iter()
            .find(|k| k.id == entry.selection.key_id)
            .context("Subagent pool references a removed key")?;
        let model = key
            .models
            .iter()
            .find(|m| m.id == entry.selection.model_id)
            .context("Subagent pool references a removed model")?;
        if key.secret.as_ref().is_none_or(|s| s.is_empty()) {
            bail!("Subagent pool key is not configured");
        }
        entries.push(json!({"model_id":entry.id,"name":entry.name,"note":entry.note,"site_name":site.name,"model_name":model.name,"model":model.model,"reasoning_effort":entry.reasoning_effort.as_ref().unwrap_or(&model.reasoning_effort),"supports_images":model.supports_images,"context_window":model.context_window,"max_output_tokens":model.max_output_tokens}));
    }
    Ok(json!({"inherit_available":true,"choices":entries}))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    fn fixture() -> (AppConfig, Session) {
        let cfg = AppConfig {
            sites: vec![Site {
                id: "s".into(),
                name: "One".into(),
                base_url: "https://one.example/v1".into(),
                keys: vec![ApiKey {
                    id: "k".into(),
                    name: "private key label".into(),
                    secret: Some("private-secret".into()),
                    has_secret: true,
                    models: vec![ModelConfig {
                        id: "m".into(),
                        name: "Model".into(),
                        model: "any-model".into(),
                        ..Default::default()
                    }],
                }],
            }],
            subagent_models: vec![SubagentModel {
                id: "choice".into(),
                name: "Reviewer".into(),
                selection: Selection {
                    site_id: "s".into(),
                    key_id: "k".into(),
                    model_id: "m".into(),
                },
                reasoning_effort: Some("high".into()),
                note: "高智商模型".into(),
            }],
            ..Default::default()
        };
        let s = Session {
            id: new_id(),
            title: "P".into(),
            cwd: "/tmp".into(),
            selection: cfg.subagent_models[0].selection.clone(),
            reasoning_effort: Some("low".into()),
            status: "idle".into(),
            created_at: now(),
            updated_at: now(),
            parent_id: None,
            depth: 0,
            context_epoch: 0,
            encryption: "none".into(),
            locked: false,
            activator_enabled: false,
            activator_prompt: String::new(),
        };
        (cfg, s)
    }
    #[test]
    fn inherit_and_explicit_choices_preserve_distinct_efforts() {
        let (cfg, parent) = fixture();
        assert_eq!(resolve(&cfg, &parent, None).unwrap().1, Some("low".into()));
        assert_eq!(
            resolve(&cfg, &parent, Some("inherit")).unwrap().1,
            Some("low".into())
        );
        assert_eq!(
            resolve(&cfg, &parent, Some("choice")).unwrap().1,
            Some("high".into())
        );
        assert!(resolve(&cfg, &parent, Some("arbitrary-vendor-model")).is_err());
    }
    #[test]
    fn default_and_omission_are_distinct_and_catalog_has_no_secrets() {
        let (mut cfg, parent) = fixture();
        cfg.subagent_models[0].reasoning_effort = None;
        assert!(resolve(&cfg, &parent, Some("choice")).unwrap().1.is_none());
        assert_eq!(
            catalog(&cfg).unwrap()["choices"][0]["reasoning_effort"],
            "medium"
        );
        cfg.subagent_models[0].reasoning_effort = Some("".into());
        assert_eq!(
            resolve(&cfg, &parent, Some("choice")).unwrap().1,
            Some("".into())
        );
        let c = catalog(&cfg).unwrap().to_string();
        assert!(
            !c.contains("private-secret")
                && !c.contains("private key label")
                && !c.contains("one.example")
        );
        assert_eq!(catalog(&cfg).unwrap()["choices"][0]["reasoning_effort"], "");
    }
}
