//! "Save as skill": the draft comes from the run's steps, never copies secrets, and nothing is
//! installed until the owner confirms the reviewed fingerprint.
use super::management::setup_production_redactor_with;
use super::*;
use crate::tests::{call_token_only, make_chat};
use protocol::{BotId, ChatId, RunId};
use uuid::Uuid;

const CANARY: &str = "sk-live-CANARY9f3a77c2d1e84b6a90";

/// A finished run of `chat` with one successful `web_fetch` and one failed `web_search`.
fn record_run_with(t: &T, bot: &str, chat: &str, args: &Value, tainted: bool) {
    let st = t.state.storage();
    let run = RunId::new();
    st.insert_run(&storage::Run {
        id: run,
        user_id: t.state.user_id(),
        bot_id: BotId(bot.parse().unwrap()),
        chat_id: Some(ChatId(chat.parse().unwrap())),
        kind: "turn".into(),
        state: "succeeded".into(),
        error: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        parent_run_id: None,
        spec: None,
        computer_target: None,
        computer_node: None,
        computer_node_name: None,
        paused_at_ms: None,
        pause_reason: None,
    })
    .unwrap();
    st.insert_step(&storage::RunStep {
        id: Uuid::now_v7(),
        run_id: run,
        user_id: t.state.user_id(),
        idx: 0,
        name: "tools".into(),
        input: json!({"calls": [
            {"id": "c1", "name": "web_fetch", "arguments": args.to_string(), "repeat": 1},
            {"id": "c2", "name": "web_search", "arguments": "{}", "repeat": 1}
        ]}),
        idempotent: true,
        retry: None,
        status: "succeeded".into(),
        attempt: 1,
        // c2 failed, so it must not become a step.
        output: Some(json!({"results": [
            {"id": "c1", "is_error": false, "text": "ok"},
            {"id": "c2", "is_error": true, "text": "boom"}
        ]})),
        error: None,
        started_at_ms: 0,
        finished_at_ms: Some(1),
    })
    .unwrap();
    if tainted {
        st.insert_message(&storage::Message {
            id: Uuid::now_v7(),
            user_id: t.state.user_id(),
            chat_id: ChatId(chat.parse().unwrap()),
            run_id: Some(run),
            role: "assistant".into(),
            content: "read a web page".into(),
            created_at_ms: 1,
            exposure_untrusted: true,
            exposure_private: false,
            exposure_sources: vec!["web".into()],
        })
        .unwrap();
    }
}

fn record_run(t: &T, bot: &str, chat: &str) {
    let args = json!({"url": "https://example.com/docs", "api_key": CANARY, "q": CANARY});
    record_run_with(t, bot, chat, &args, false);
}

#[tokio::test]
async fn draft_has_no_secret_installs_only_after_confirmation_and_is_owner_only() {
    let t = setup_skills(|_| {});
    let (bot, chat) = make_chat(&t).await;
    let path = format!("/v1/chats/{chat}/skill-draft");

    // Nothing finished yet: an honest, specific refusal.
    let r = call(&t, Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "skill_draft_no_run");

    record_run(&t, &bot, &chat);
    let r = call_token_only(&t, Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = call(&t, Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let d = r.json();
    assert!(!d.to_string().contains(CANARY), "secret leaked into draft");
    assert_eq!(d["steps"].as_array().unwrap().len(), 1, "failed call kept");
    assert_eq!(d["tools"][0]["name"], "web_fetch");
    assert!(d["withheld"].as_u64().unwrap() >= 3);
    let skill_md = d["skill_md"].as_str().unwrap().to_owned();
    let name = d["name"].as_str().unwrap().to_owned();
    let fingerprint = d["preview"]["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(d["tainted"], false);

    // The draft alone installs nothing; a wrong confirmation installs nothing.
    let dir = t.state.data_dir().join("skills").join(&name);
    assert!(!dir.exists());
    let body = json!({"skill_md": skill_md, "accept": "wrong", "chat_id": chat});
    let r = call(&t, Method::POST, "/v1/skills/draft/install", Some(body)).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(!dir.exists());

    let body = json!({"skill_md": skill_md, "accept": fingerprint, "chat_id": chat});
    let r = call_token_only(
        &t,
        Method::POST,
        "/v1/skills/draft/install",
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&t, Method::POST, "/v1/skills/draft/install", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let saved = fs::read_to_string(dir.join("SKILL.md")).unwrap();
    assert!(!saved.contains(CANARY));
    assert!(saved.contains("web_fetch"));
}

/// A value the vault knows, typed under harmless key names, and one the owner edits in.
#[tokio::test]
async fn registered_vault_values_under_neutral_keys_are_withheld_and_edits_with_them_are_refused() {
    const VAULT: &str = "Hunter2!x";
    const CODE: &str = "482913";
    let t = setup_production_redactor_with(&[VAULT]);
    let (bot, chat) = make_chat(&t).await;
    // `text` holds a vault value, `code` a one-time code, `phone` a number; the URL has a token.
    let args = json!({
        "text": VAULT, "code": CODE, "phone": "+905321234567",
        "url": "https://x.example/reset/ab12cd34", "limit": 5
    });
    record_run_with(&t, &bot, &chat, &args, false);

    let r = call(
        &t,
        Method::POST,
        &format!("/v1/chats/{chat}/skill-draft"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let d = r.json();
    let dump = d.to_string();
    for forbidden in [VAULT, CODE, "+905321234567", "ab12cd34"] {
        assert!(!dump.contains(forbidden), "{forbidden} leaked: {dump}");
    }
    assert!(dump.contains("{{text}}") && dump.contains("{{code}}"));
    let skill_md = d["skill_md"].as_str().unwrap().to_owned();
    let name = d["name"].as_str().unwrap().to_owned();
    let fingerprint = d["preview"]["fingerprint"].as_str().unwrap().to_owned();

    // The owner pastes the vault value into the previewed text: refused at preview and install.
    let edited = format!("{skill_md}\nPassword is {VAULT}\n");
    let dir = t.state.data_dir().join("skills").join(&name);
    for (route, body) in [
        ("/v1/skills/draft/preview", json!({"skill_md": edited})),
        (
            "/v1/skills/draft/install",
            json!({"skill_md": edited, "accept": fingerprint, "chat_id": chat,
                   "acknowledged": true}),
        ),
    ] {
        let r = call(&t, Method::POST, route, Some(body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{route}");
        assert_eq!(
            r.json()["error"]["code"],
            "skill_contains_secret",
            "{route}"
        );
        assert!(!String::from_utf8_lossy(&r.body).contains(VAULT));
        assert!(!dir.exists(), "{route} wrote the skill");
    }

    // The untouched draft installs.
    let body = json!({"skill_md": skill_md, "accept": fingerprint, "chat_id": chat});
    let r = call(&t, Method::POST, "/v1/skills/draft/install", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let saved = fs::read_to_string(dir.join("SKILL.md")).unwrap();
    for forbidden in [VAULT, CODE, "+905321234567", "ab12cd34"] {
        assert!(!saved.contains(forbidden), "{forbidden} saved");
    }
}

/// The tick in the dialog is not the only gate: the daemon re-checks the run's exposure.
#[tokio::test]
async fn install_from_a_tainted_run_needs_the_acknowledgement_server_side() {
    let t = setup_skills(|_| {});
    let (bot, chat) = make_chat(&t).await;
    let args = json!({"url": "https://example.com"});
    record_run_with(&t, &bot, &chat, &args, true);

    let d = call(
        &t,
        Method::POST,
        &format!("/v1/chats/{chat}/skill-draft"),
        None,
    )
    .await
    .json();
    assert_eq!(d["tainted"], true);
    let skill_md = d["skill_md"].as_str().unwrap().to_owned();
    let name = d["name"].as_str().unwrap().to_owned();
    let accept = d["preview"]["fingerprint"].as_str().unwrap().to_owned();
    let dir = t.state.data_dir().join("skills").join(&name);

    let route = "/v1/skills/draft/install";
    let body = json!({"skill_md": skill_md, "accept": accept, "chat_id": chat});
    let r = call(&t, Method::POST, route, Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "skill_draft_ack_required");
    let mut explicit_no = body.clone();
    explicit_no["acknowledged"] = json!(false);
    let r = call(&t, Method::POST, route, Some(explicit_no)).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(!dir.exists());

    // Another chat id cannot be used to dodge the check.
    let mut other = body.clone();
    other["chat_id"] = json!(Uuid::now_v7().to_string());
    other["acknowledged"] = json!(true);
    let r = call(&t, Method::POST, route, Some(other)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let mut no_chat = body.clone();
    no_chat.as_object_mut().unwrap().remove("chat_id");
    let r = call(&t, Method::POST, route, Some(no_chat)).await;
    assert!(r.status.is_client_error());
    assert!(!dir.exists());

    let mut ok = body;
    ok["acknowledged"] = json!(true);
    let r = call(&t, Method::POST, route, Some(ok)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    assert!(dir.join("SKILL.md").exists());
}
