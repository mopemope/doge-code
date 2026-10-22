use super::*;

#[test]
fn openrouter_endpoint_and_model_id_are_strict() {
    for base in [BASE_URL, "https://OPENROUTER.AI:443/api/v1/"] {
        assert!(validate_base(base).is_ok(), "{base}");
    }
    for base in [
        "http://openrouter.ai/api/v1",
        "https://openrouter.ai:444/api/v1",
        "https://openrouter.ai.example.invalid/api/v1",
        "https://user@openrouter.ai/api/v1",
        "https://openrouter.ai/api/v1?token=x",
        "https://openrouter.ai/api/v1#x",
        "https://openrouter.ai/api/v2",
    ] {
        assert!(validate_base(base).is_err(), "{base}");
    }
    for id in [
        "publisher/model:free",
        "~publisher/model",
        "Publisher/model:v1",
        &format!("a/{}", "x".repeat(254)),
    ] {
        assert_eq!(model_id(id).expect("valid ID"), id);
    }
    for id in [
        "publisher/",
        "/model",
        "publisher/model\n",
        "publisher/model?x",
        "a@b/model",
        "model",
        &format!("a/{}", "x".repeat(255)),
    ] {
        assert!(model_id(id).is_err(), "{id}");
    }
    assert_eq!(
        inference_binding("Publisher/model:free").unwrap(),
        "Openrouter:https://openrouter.ai/api/v1:ChatCompletions:Publisher/model:free"
    );
}

#[test]
fn openrouter_selection_config_preserves_origin_and_rejects_foreign_metadata() {
    let mut current = AppConfig {
        api_key: Some("synthetic-other".into()),
        api_key_origin: Some(crate::config::ConfigOrigin::Cli),
        ..Default::default()
    };
    let cfg = selection_config(&current, "Publisher/model:free", None, None).unwrap();
    assert_eq!(cfg.api_key, None);
    assert_eq!(cfg.api_key_origin, None);
    assert_eq!(cfg.model, "Publisher/model:free");
    assert_eq!(cfg.base_url, BASE_URL);
    assert_eq!(current.api_key.as_deref(), Some("synthetic-other"));
    current.provider = ProviderKind::Openrouter;
    current.base_url = BASE_URL.into();
    let cfg = selection_config(&current, "p/next", Some("synthetic-env".into()), None).unwrap();
    assert_eq!(cfg.api_key, current.api_key);
    assert_eq!(cfg.api_key_origin, current.api_key_origin);
    assert!(cfg.openrouter_metadata.is_none());
    current.api_key = None;
    current.api_key_origin = None;
    let cfg = selection_config(&current, "p/next", Some("synthetic-env".into()), None).unwrap();
    assert!(
        cfg.api_key.is_none(),
        "same-provider key resolution must be retained"
    );
    let metadata = Arc::new(SelectedModelMetadata {
        provider: ProviderKind::OpenaiCompatible,
        fetched_at: Utc::now(),
        model: OpenRouterModel {
            id: "p/next".into(),
            ..Default::default()
        },
    });
    assert!(
        selection_config(&current, "p/next", None, Some(metadata))
            .unwrap()
            .openrouter_metadata
            .is_none()
    );
}
