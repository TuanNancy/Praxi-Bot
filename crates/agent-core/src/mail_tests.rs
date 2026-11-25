//! A bot that has a mail identity knows it (a fixed fact block in every run), and
//! the two calls of a sign-up that the taint gate would ask about (reading its
//! own inbox, opening the link the mail tool returned as followable) run without
//! a card only for an owner-started run on the Full level.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use builtin_tools::connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, EmailConnector, IdentitySettings, ImapSettings,
    SendMode, SmtpSettings,
};
use permission_engine::Policy;
use protocol::{BotId, Event, PermissionCategory};
use secret_store::{MemoryStore, SecretRef};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, ContentPart, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::tests::{
    Fake, TestTool, calls, cfg, make_loop, profile, setup, t, text, until, with_class,
};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
const GOOD: &str = "https://app.example.com/verify?token=abc";
const EVIL: &str = "https://evil.example.net/steal";

pub(crate) fn account(
    name: &str,
    bot: Option<String>,
    address: &str,
    mode: SendMode,
) -> AccountConfig {
    AccountConfig {
        name: name.into(),
        address: address.into(),
        username: String::new(),
        password: SecretRef::new(format!("email/{name}/api_key")).unwrap(),
        imap: ImapSettings::default(),
        smtp: SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot,
        allowed_senders: Vec::new(),
        identity: IdentitySettings {
            send_mode: mode,
            ..IdentitySettings::default()
        },
        kind: AccountKind::AgentMail,
        inbox_id: Some(address.into()),
    }
}

pub(crate) fn connector(accounts: Vec<AccountConfig>) -> Arc<EmailConnector> {
    let c = EmailConnector::reloadable_with(
        Arc::new(MemoryStore::new()),
        AgentMailEndpoint::loopback_mock_for_tests("http://127.0.0.1:9").unwrap(),
    );
    c.replace_accounts(accounts).unwrap();
    Arc::new(c)
}

/// Stands in for `email_wait_for`: same spec shape, answers with one followable link.
struct WaitFake {
    spec: ToolSpec,
    count: Arc<AtomicUsize>,
}

fn wait_fake() -> Arc<WaitFake> {
    Arc::new(WaitFake {
        spec: ToolSpec {
            name: builtin_tools::mail_wait::EMAIL_WAIT_FOR.into(),
            description: "wait for mail".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Messaging,
            platforms: vec![Platform::All],
            content: ContentClass::all(),
        },
        count: Arc::new(AtomicUsize::new(0)),
    })
}

#[async_trait]
impl Tool for WaitFake {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: json!({"found": true, "followable_links": [GOOD], "codes": []}),
            }],
            spill: None,
            exposure: None,
        })
    }
}

fn open_tool() -> Arc<TestTool> {
    Arc::new(with_class(
        "browser_open",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::all(),
        "opened",
    ))
}

fn page_tool() -> Arc<TestTool> {
    Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "a sign-up page",
    ))
}

/// Run: read a page, wait for the mail, open the followable link, open another
/// link. Returns the tool and URL of the first call that asked, or `None` when
/// the run finished without asking.
async fn first_ask(source: &'static str, own: bool, extra: &str) -> Option<(String, Value)> {
    let env = setup();
    let bot = env.bot.to_string();
    let conn = connector(vec![account(
        "bot-mail",
        own.then(|| bot.clone()),
        "me@agentmail.to",
        SendMode::Ask,
    )]);
    let page = page_tool();
    let open = open_tool();
    let wait = wait_fake();
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "page.html"}))]),
        calls(&[("c2", "email_wait_for", json!({}))]),
        calls(&[("c3", "browser_open", json!({"url": GOOD}))]),
        calls(&[("c4", "browser_open", json!({"url": EVIL}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&open)],
        profile(extra),
        cfg(Policy::full()),
    );
    lp.tool_registry()
        .register(Arc::clone(&wait) as Arc<dyn Tool>);
    lp.mail_handle().set(conn);
    let mut rx = lp.subscribe();
    lp.send_user_message_from(env.chat, "sign up", crate::MessageMode::Steer, source)
        .unwrap();
    let ev = until(&mut rx, |e| {
        matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
    })
    .await;
    match ev {
        Event::ApprovalNeeded(a) => Some((a.tool_name, a.input)),
        _ => None,
    }
}

#[tokio::test]
async fn an_owner_run_on_full_reads_its_own_inbox_and_opens_the_followable_link_without_a_card() {
    // The first thing that asks is the OTHER link: neither the inbox read nor the
    // link the mail tool returned needed a card.
    let (tool, input) = first_ask("owner", true, FULL)
        .await
        .expect("the evil link asks");
    assert_eq!(tool, "browser_open");
    assert_eq!(input["url"], EVIL);
}

#[tokio::test]
async fn a_run_that_is_not_owner_started_keeps_asking_for_the_inbox_read() {
    for source in ["imported", "telegram"] {
        let (tool, _) = first_ask(source, true, FULL).await.expect("asks");
        assert_eq!(tool, "email_wait_for", "{source}");
    }
}

#[tokio::test]
async fn an_inbox_every_bot_may_use_is_not_the_bots_own_and_still_asks() {
    let (tool, _) = first_ask("owner", false, FULL).await.expect("asks");
    assert_eq!(tool, "email_wait_for");
}

#[tokio::test]
async fn below_full_nothing_is_waived() {
    let (tool, _) = first_ask("owner", true, "").await.expect("asks");
    assert_eq!(tool, "email_wait_for");
}

/// The system prompt of the first model call of a run of `bot` with `accounts`.
async fn system_prompt(
    accounts: impl FnOnce(&BotId) -> Vec<AccountConfig>,
    tools: &[&str],
) -> String {
    let env = setup();
    let conn = connector(accounts(&env.bot));
    let fake = Fake::new(vec![text("hi")]);
    let made: Vec<Arc<TestTool>> = tools
        .iter()
        .map(|n| {
            Arc::new(with_class(
                n,
                Effect::Read,
                PermissionCategory::Fs,
                ContentClass::none(),
                "x",
            ))
        })
        .collect();
    let refs: Vec<&Arc<TestTool>> = made.iter().collect();
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &refs,
        profile(""),
        cfg(Policy::full()),
    );
    lp.mail_handle().set(conn);
    let run = lp.run_turn(env.chat, "hello").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    fake.request(0).messages[0]
        .content
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_bot_with_an_identity_is_told_its_address_and_how_to_read_it() {
    let sys = system_prompt(
        |bot| {
            vec![account(
                "bot-mail",
                Some(bot.to_string()),
                "me@agentmail.to",
                SendMode::DraftOnly,
            )]
        },
        &["email_search", "email_read", "email_wait_for", "email_send"],
    )
    .await;
    assert!(sys.contains("Your mail identity:"), "{sys}");
    assert!(sys.contains("me@agentmail.to"), "{sys}");
    assert!(sys.contains("send mode: draft only"), "{sys}");
    assert!(sys.contains("verification mails"), "{sys}");
    assert!(sys.contains("email_wait_for"), "{sys}");
}

#[tokio::test]
async fn a_bot_without_an_identity_gets_no_mail_block_and_no_other_bots_address() {
    let sys = system_prompt(
        |_| {
            vec![
                account(
                    "other-bots-mail",
                    Some(BotId::new().to_string()),
                    "secret-other@agentmail.to",
                    SendMode::Ask,
                ),
                // An inbox every bot may use is not this bot's identity either.
                account("pool", None, "pool@agentmail.to", SendMode::Ask),
            ]
        },
        &["email_search", "email_read"],
    )
    .await;
    assert!(!sys.contains("Your mail identity"), "{sys}");
    assert!(!sys.contains("secret-other@agentmail.to"), "{sys}");
    assert!(!sys.contains("pool@agentmail.to"), "{sys}");
}

#[tokio::test]
async fn the_block_does_not_promise_tools_the_bot_does_not_have() {
    let sys = system_prompt(
        |bot| {
            vec![account(
                "bot-mail",
                Some(bot.to_string()),
                "me@agentmail.to",
                SendMode::Ask,
            )]
        },
        &["fs_read"],
    )
    .await;
    assert!(sys.contains("me@agentmail.to"), "{sys}");
    assert!(sys.contains("switched off"), "{sys}");
    assert!(!sys.contains("email_wait_for"), "{sys}");
}
