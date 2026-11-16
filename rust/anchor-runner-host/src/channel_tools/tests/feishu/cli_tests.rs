use super::*;

#[tokio::test]
async fn cli_requires_explicit_own_bot_grant_and_active_turn_and_stages_visible_files() {
    let root = tempfile::tempdir().unwrap();
    let FeishuFixture {
        host,
        channels,
        listener,
        sessions,
        session,
        ..
    } = feishu_fixture(root.path());
    let args = json!({"argv":["skills","list"]});
    assert!(
        !host
            .definitions()
            .iter()
            .any(|tool| tool.name == FEISHU_CLI_TOOL)
    );
    assert!(
        host.call(FEISHU_CLI_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("not authorized")
    );
    channels.update("graph", |bot| bot.binding.cli_enabled = true);
    let context = host
        .call(FEISHU_CLI_TOOL, json!({"argv":["context"]}))
        .await
        .unwrap();
    assert!(
        serde_json::to_value(context)
            .unwrap()
            .to_string()
            .contains("oc_private")
    );
    assert!(
        host.definitions()
            .iter()
            .any(|tool| tool.name == FEISHU_CLI_TOOL)
    );
    assert!(!host.is_read_only(FEISHU_CLI_TOOL));
    assert!(
        host.call(FEISHU_CLI_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("Goose tool identity")
    );
    fs::write(host.workspace.join("input.md"), "hello").unwrap();
    let identity = GooseToolIdentity {
        key: host.key.clone(),
        session: "native".into(),
        tool_call: "cli-1".into(),
    };
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        reader.read_until(b'\n', &mut bytes).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["operation"], "cli");
        assert_eq!(request["token"], "private-feishu-token");
        assert_eq!(
            request["invocation"]["files"][0],
            json!({"name":"input.md","data_base64":STANDARD.encode("hello")})
        );
        assert!(request.get("app_id").is_none());
        reader.get_mut().write_all(format!("{}\n", json!({"accepted":true,"request_id":request["request_id"],"exit_code":0,"stdout":"{}","outputs":[{"name":"result.md","data_base64":STANDARD.encode("result") }]})).as_bytes()).await.unwrap();
    };
    let call = scope(Some(identity.clone()), host.call(FEISHU_CLI_TOOL,
        json!({"argv":["docs","+create"],"files":[{"path":"/workspace/input.md","name":"input.md"}],"outputs":["result.md"]})));
    let (result, ()) = tokio::join!(call, server);
    let text = serde_json::to_value(result.unwrap()).unwrap().to_string();
    assert!(!text.contains("data_base64"));
    assert!(text.contains("result.md"));
    assert!(
        fs::read_dir(&host.workspace).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .join("result.md")
            .is_file())
    );
    for files in [
        json!([{"path":"/etc/passwd","name":"p"}]),
        json!([{"path":"/workspace/../escape","name":"p"}]),
    ] {
        assert!(
            scope(
                Some(identity.clone()),
                host.call(FEISHU_CLI_TOOL, json!({"argv":["--help"],"files":files}))
            )
            .await
            .is_err()
        );
    }
    channels.update("graph", |bot| bot.owner = "other".into());
    assert!(
        scope(
            Some(identity.clone()),
            host.call(FEISHU_CLI_TOOL, args.clone())
        )
        .await
        .is_err()
    );
    channels.update("graph", |bot| bot.owner = "local".into());
    let input = crate::assistant::input(root.path(), &host.key)
        .unwrap()
        .unwrap();
    sessions
        .finish_turn(
            "local",
            &session,
            &input.turn().id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert!(
        scope(Some(identity), host.call(FEISHU_CLI_TOOL, args))
            .await
            .unwrap_err()
            .to_string()
            .contains("inactive")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}
