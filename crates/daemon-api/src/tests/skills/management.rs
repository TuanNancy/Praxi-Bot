//! Positive and negative tests drive the real router and shared store.
use super::*;
use crate::tests::{TOKEN, send};

fn document(name: &str, instructions: &str) -> Value {
    json!({"name": name, "description": "Owner-authored skill", "instructions": instructions})
}

#[tokio::test]
async fn owner_skill_crud_is_live_and_catalog_exposes_local_locations() {
    let t = setup_skills(|_| {});
    let response = call(
        &t,
        Method::POST,
        "/v1/skills",
        Some(document("hello", "Original body")),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::CREATED,
        "{:?}",
        response.json()
    );
    assert_eq!(response.json()["scope"], "shared");
    let bot = create_bot(&t).await;
    let index = t.state.skills_index().unwrap();
    assert_eq!(
        index
            .for_bot(Some(&bot))
            .unwrap()
            .get("hello")
            .unwrap()
            .body,
        "Original body"
    );
    let catalog = call(&t, Method::GET, "/v1/skills", None).await.json();
    assert_eq!(
        catalog["locations"]["shared"],
        t.state.data_dir().join("skills").to_string_lossy().as_ref()
    );
    assert!(
        catalog["locations"]["bot_template"]
            .as_str()
            .unwrap()
            .contains("{bot_id}")
    );
    let response = call(
        &t,
        Method::PUT,
        "/v1/skills/hello",
        Some(document("hello", "Updated body")),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
    assert_eq!(response.json()["version"], "0.1.1");
    assert_eq!(
        index
            .for_bot(Some(&bot))
            .unwrap()
            .get("hello")
            .unwrap()
            .body,
        "Updated body"
    );
    let conflict = call(&t, Method::DELETE, "/v1/skills/hello", None).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.json()["used_by"][0]["id"], bot);
    assert!(t.state.data_dir().join("skills/hello/SKILL.md").exists());
    let deleted = call(&t, Method::DELETE, "/v1/skills/hello?force=true", None).await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(deleted.json()["deleted"], true);
    assert!(index.for_bot(Some(&bot)).unwrap().get("hello").is_none());
    // A bot with implicit All must not turn a missing skill into an
    // assignment conflict, nor imply that bundled skills can be removed.
    let missing = call(&t, Method::DELETE, "/v1/skills/hello", None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn owner_imports_folder_without_executing_or_returning_content() {
    let t = setup_skills(|dir| {
        write_skill(
            &dir.join("source"),
            "imported",
            "Imported",
            "DO_NOT_ECHO_BODY",
            None,
        );
        fs::write(
            dir.join("source/imported/helper.py"),
            "raise RuntimeError('MUST_NOT_EXECUTE')",
        )
        .unwrap();
    });
    let response = call(
        &t,
        Method::POST,
        "/v1/skills/import",
        Some(json!({
            "kind": "folder", "path": t.state.data_dir().join("source/imported").to_string_lossy()
        })),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::CREATED,
        "{:?}",
        response.json()
    );
    assert!(
        t.state
            .data_dir()
            .join("skills/imported/helper.py")
            .exists()
    );
    assert!(!String::from_utf8_lossy(&response.body).contains("DO_NOT_ECHO_BODY"));
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let rows: Vec<_> = audit
        .into_iter()
        .filter(|r| r.action.starts_with("skill."))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].detail, json!({"files":2}));
}

#[tokio::test]
async fn zip_upload_is_decoded_bounded_and_published_through_the_real_route() {
    use base64::Engine as _;
    let t = setup_skills(|_| {});
    let bytes = include_bytes!("import-fixture.zip");
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let result = call(
        &t,
        Method::POST,
        "/v1/skills/import",
        Some(json!({"kind":"zip", "base64":encoded})),
    )
    .await;
    assert_eq!(result.status, StatusCode::CREATED, "{:?}", result.json());
    assert_eq!(result.json()["name"], "zip-fixture");
    assert_eq!(
        fs::read_to_string(t.state.data_dir().join("skills/zip-fixture/refs/note.txt")).unwrap(),
        "inert reference"
    );
    let invalid = call(
        &t,
        Method::POST,
        "/v1/skills/import",
        Some(json!({"kind":"zip", "base64":"PRIVATE_NOT_BASE64"})),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!String::from_utf8_lossy(&invalid.body).contains("PRIVATE_NOT_BASE64"));
}

#[tokio::test]
async fn all_peer_roles_are_refused_skill_mutations_for_path_variants() {
    let t = setup_skills(|dir| write_skill(&dir.join("skills"), "hello", "Hello", "Old", None));
    // The same actual enabled handler works for the local owner first.
    assert_eq!(
        call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document("owner-positive", "Body"))
        )
        .await
        .status,
        StatusCode::CREATED
    );
    for role in [
        PeerRole::Desktop,
        PeerRole::Phone,
        PeerRole::Node,
        PeerRole::Host,
    ] {
        let listing = Request::builder()
            .uri("/v1/skills")
            .extension(PeerIdentity {
                key: "paired-test-peer".into(),
                role,
            })
            .body(Body::empty())
            .unwrap();
        let response = peer_router(t.state.clone()).oneshot(listing).await.unwrap();
        let expected = if matches!(role, PeerRole::Desktop | PeerRole::Phone) {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(response.status(), expected);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.get("locations").is_none());
        for (method, suffix) in [
            (Method::POST, ""),
            (Method::POST, "/import"),
            (Method::PUT, "/hello"),
            (Method::DELETE, "/hello"),
        ] {
            for prefix in ["/v1/skills", "/v1/%73kills", "/v1%2Fskills", "//v1//skills"] {
                for trailing in ["", "/"] {
                    let path = format!("{prefix}{suffix}{trailing}");
                    let request = Request::builder()
                        .method(method.clone())
                        .uri(&path)
                        .header("content-type", "application/json")
                        .extension(PeerIdentity {
                            key: "paired-test-peer".into(),
                            role,
                        })
                        .body(Body::from(document("hello", "Replaced").to_string()))
                        .unwrap();
                    let response = peer_router(t.state.clone()).oneshot(request).await.unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::FORBIDDEN,
                        "{role:?} {method} {path}"
                    );
                }
            }
        }
    }
    assert!(
        fs::read_to_string(t.state.data_dir().join("skills/hello/SKILL.md"))
            .unwrap()
            .contains("Old")
    );
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|r| r.action.starts_with("skill."))
            .count(),
        1
    );
}

#[tokio::test]
async fn local_owner_mutations_still_require_bearer_and_reject_spoofed_peer_headers() {
    let t = setup_skills(|_| {});
    let no_token = send(
        &t.app,
        Method::POST,
        "/v1/skills",
        Some(document("hello", "Body")),
        &[],
    )
    .await;
    assert_eq!(no_token.status, StatusCode::UNAUTHORIZED);
    let valid = send(
        &t.app,
        Method::POST,
        "/v1/skills",
        Some(document("hello", "Body")),
        &[
            ("authorization", &format!("Bearer {TOKEN}")),
            ("x-peer-role", "desktop"),
        ],
    )
    .await;
    assert_eq!(valid.status, StatusCode::CREATED);
}

#[tokio::test]
async fn skill_mutations_fail_closed_before_disk_when_audit_fails() {
    let t = setup_skills(|dir| {
        write_skill(
            &dir.join("skills"),
            "existing",
            "Existing",
            "ORIGINAL",
            None,
        );
        write_skill(&dir.join("source"), "imported", "Imported", "IMPORT", None);
    });
    let db = rusqlite::Connection::open(t.state.data_dir().join("bot.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_skill_audit BEFORE INSERT ON audit_log
        BEGIN SELECT RAISE(ABORT, 'PRIVATE_DATABASE_ERROR'); END;",
    )
    .unwrap();
    let requests = [
        (Method::POST, "/v1/skills", Some(document("created", "NEW"))),
        (
            Method::PUT,
            "/v1/skills/existing",
            Some(document("existing", "UPDATED")),
        ),
        (Method::DELETE, "/v1/skills/existing?force=true", None),
        (
            Method::POST,
            "/v1/skills/import",
            Some(
                json!({"kind":"folder", "path":t.state.data_dir().join("source/imported").to_string_lossy()}),
            ),
        ),
    ];
    for (method, path, body) in requests {
        let response = call(&t, method, path, body).await;
        assert_eq!(
            response.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{:?}",
            response.json()
        );
        assert!(!String::from_utf8_lossy(&response.body).contains("PRIVATE_DATABASE_ERROR"));
    }
    assert!(!t.state.data_dir().join("skills/created").exists());
    assert!(!t.state.data_dir().join("skills/imported").exists());
    assert!(
        fs::read_to_string(t.state.data_dir().join("skills/existing/SKILL.md"))
            .unwrap()
            .contains("ORIGINAL")
    );
}

#[tokio::test]
async fn invalid_names_and_parser_errors_do_not_echo_user_content() {
    let t = setup_skills(|_| {});
    for name in [
        "../outside",
        "con",
        "aux.txt",
        "import",
        "UpperCase",
        "x..y",
        "nul",
    ] {
        let response = call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document(name, "BODY_SECRET_SENTINEL")),
        )
        .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        assert!(!String::from_utf8_lossy(&response.body).contains("BODY_SECRET_SENTINEL"));
    }
    let bad = call(
        &t,
        Method::POST,
        "/v1/skills",
        Some(json!({"skill_md":"---\nSECRET_PARSER_KEY: value\n---\nBody"})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!String::from_utf8_lossy(&bad.body).contains("SECRET_PARSER_KEY"));
}

#[tokio::test]
async fn simultaneous_api_creates_have_one_success_one_conflict() {
    let t = setup_skills(|_| {});
    let (a, b) = tokio::join!(
        call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document("same", "ONE"))
        ),
        call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document("same", "TWO"))
        ),
    );
    let mut statuses = [a.status.as_u16(), b.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409]);
}

#[tokio::test]
async fn selected_assignments_are_preserved_as_missing_after_force_delete() {
    let t = setup_skills(|_| {});
    assert_eq!(
        call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document("chosen", "Body"))
        )
        .await
        .status,
        StatusCode::CREATED
    );
    let bot = create_bot(&t).await;
    let route = format!("/v1/bots/{bot}/skills");
    assert_eq!(
        call(
            &t,
            Method::PUT,
            &route,
            Some(json!({"mode":"selected","names":["chosen"]}))
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&t, Method::DELETE, "/v1/skills/chosen", None)
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&t, Method::DELETE, "/v1/skills/chosen?force=true", None)
            .await
            .status,
        StatusCode::OK
    );
    let result = call(&t, Method::GET, &route, None).await.json();
    assert_eq!(
        result["assigned"],
        json!([{"name":"chosen","state":"missing"}])
    );
}

fn setup_production_redactor(secret: &str) -> T {
    setup_production_redactor_with(&[secret])
}

/// A daemon whose redactor knows every value in `secrets`, like registered vault values.
pub(super) fn setup_production_redactor_with(secrets: &[&str]) -> T {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
    let redactor = agent_core::SharedRedactor::default();
    for secret in secrets {
        redactor.register(secret);
    }
    let agent = agent_core::AgentLoop::new(
        Arc::clone(&storage),
        tool_registry::ToolRegistry::new(),
        agent_core::Providers::single(Arc::new(crate::fake::FakeProvider)),
        agent_core::LoopConfig {
            redactor: Some(redactor),
            ..Default::default()
        },
    );
    let state = AppState::new(agent.clone(), storage, crate::DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key("ownerkey0123456789abcdef0123456789abcdef0123456789abcdef".into());
    let index = ::skills::SkillIndex::load_roots(::skills::SkillRoots::from_data_dir(dir.path()))
        .with_assignments(Arc::new(SkillAssignments::new(state.storage_arc())));
    state.set_skills(index).unwrap();
    T {
        app: crate::router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

#[tokio::test]
async fn production_redactor_rejects_secrets_in_documents_and_imported_extras() {
    const SECRET: &str = "env-only-skill-credential-K19";
    let t = setup_production_redactor(SECRET);
    let positive = call(
        &t,
        Method::POST,
        "/v1/skills",
        Some(document("positive", "Safe instructions")),
    )
    .await;
    assert_eq!(positive.status, StatusCode::CREATED);
    let bad = call(
        &t,
        Method::POST,
        "/v1/skills",
        Some(document("secret-doc", SECRET)),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!String::from_utf8_lossy(&bad.body).contains(SECRET));
    write_skill(
        &t.state.data_dir().join("source"),
        "secret-import",
        "Import",
        "Safe instructions",
        None,
    );
    fs::write(
        t.state
            .data_dir()
            .join("source/secret-import/reference.txt"),
        SECRET,
    )
    .unwrap();
    let imported = call(&t, Method::POST, "/v1/skills/import", Some(json!({
        "kind":"folder", "path":t.state.data_dir().join("source/secret-import").to_string_lossy()
    }))).await;
    assert_eq!(imported.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!t.state.data_dir().join("skills/secret-import").exists());
    assert!(!t.state.data_dir().join("skills/secret-doc").exists());
    write_skill(
        &t.state.data_dir().join("skills"),
        "legacy",
        SECRET,
        "Legacy body",
        None,
    );
    t.state.skills_index().unwrap().reload();
    let catalog = call(&t, Method::GET, "/v1/skills", None).await;
    assert!(!String::from_utf8_lossy(&catalog.body).contains(SECRET));
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    assert!(!format!("{audit:?}").contains(SECRET));
}

#[tokio::test]
async fn form_descriptions_round_trip_as_scalars_on_create_update_and_list() {
    let t = setup_skills(|_| {});
    for (i, description) in [
        "[CSV] reports",
        "\"Quoted\"",
        "C:\\folder\\notes",
        "A: B # comment",
        "Türkçe açıklama",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("description-{i}");
        let body = json!({"name":name, "description":description, "instructions":"Body"});
        let created = call(&t, Method::POST, "/v1/skills", Some(body.clone())).await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "{description}: {:?}",
            created.json()
        );
        assert_eq!(created.json()["description"], description);
        let updated = call(&t, Method::PUT, &format!("/v1/skills/{name}"), Some(body)).await;
        assert_eq!(updated.status, StatusCode::OK);
        assert_eq!(updated.json()["description"], description);
        let catalog = call(&t, Method::GET, "/v1/skills", None).await.json();
        let row = catalog["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap();
        assert_eq!(row["description"], description);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn implicit_all_assignment_on_bot_creation_uses_the_deletion_lock() {
    let t = setup_skills(|_| {});
    assert_eq!(
        call(
            &t,
            Method::POST,
            "/v1/skills",
            Some(document("assigned", "Body"))
        )
        .await
        .status,
        StatusCode::CREATED
    );
    let state = t.state.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = state.0.skill_mutations.lock();
        ready_tx.send(()).unwrap();
        release_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let app = t.app.clone();
    let mut creation = tokio::spawn(async move {
        send(
            &app,
            Method::POST,
            "/v1/bots",
            Some(json!({"profile_toml":PROFILE})),
            &[("authorization", &format!("Bearer {TOKEN}"))],
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut creation)
            .await
            .is_err(),
        "bot creation bypassed the skill deletion/assignment lock"
    );
    release_tx.send(()).unwrap();
    assert_eq!(creation.await.unwrap().status, StatusCode::CREATED);
    holder.join().unwrap();
    assert_eq!(
        call(&t, Method::DELETE, "/v1/skills/assigned", None)
            .await
            .status,
        StatusCode::CONFLICT
    );
}
