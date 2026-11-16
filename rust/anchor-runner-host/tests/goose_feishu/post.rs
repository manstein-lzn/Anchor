//! One real Host/GraphRunner/Goose turn; only platform/model transport is fake.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use std::io::Cursor;

const IMAGE_KEYS: [&str; 2] = ["img_fixture_post_red", "img_fixture_post_blue"];
const POST_TEXT: &str = "Compare these two Feishu post images.";

pub(super) fn image(key: &str) -> Vec<u8> {
    let color = match key {
        "img_fixture_post_red" => [230, 40, 10],
        "img_fixture_post_blue" => [10, 60, 220],
        unexpected => panic!("unexpected post fixture image key: {unexpected}"),
    };
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 1, Rgb(color)));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).unwrap();
    encoded.into_inner()
}

#[test]
#[ignore = "requires pinned Goose, Bubblewrap, relay and sibling Feishu Gateway; platform and provider are loopback fixtures"]
fn group_post_text_and_multiple_images_reach_native_model_and_readonly_mounts() {
    let provider = Provider::with_model(
        "goose-feishu-group-post-images",
        vec![
            command(
                "set -eu; find /in/channel -type f -name '*.png' | sort > post-images.txt; \
                 test \"$(wc -l < post-images.txt)\" -eq 2; \
                 while IFS= read -r image; do test -s \"$image\"; sha256sum \"$image\"; \
                 if (printf forbidden > \"$image\") 2>/dev/null; then exit 93; fi; \
                 done < post-images.txt; printf feishu-post-images-readonly",
            )
            .expect_request_contains(POST_TEXT),
            Step::tool(
                "final_result",
                json!({"summary":FIRST_REPLY,"route":"reply","files":[]}),
            )
            .after("feishu-post-images-readonly"),
        ],
        "gpt-4o",
    );
    let mut platform = Platform::new();
    platform.group = true;
    let host = Host::new(&json!({
        "entry":"work","ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]
    }))
    .default_runtime()
    .with_extra_environment([("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str())]);
    let (gateway, installed) = install_plugin(&host);
    let server = host.serve(&provider);
    let binding = configure_bot_members(
        &server,
        &graph(),
        &json!({
            "users":[USER],"send_users":[USER],
            "private_only":false,"groups":[GROUP_CHAT]
        }),
    );
    platform.send_content(
        1,
        FIRST_MESSAGE,
        "post",
        json!({"zh_cn":{"title":"","content":[
            [{"tag":"at","user_id":"@_user_1"},{"tag":"text","text":POST_TEXT}],
            [{"tag":"img","image_key":IMAGE_KEYS[0]},{"tag":"img","image_key":IMAGE_KEYS[1]}]
        ]}}),
    );
    let ack = platform.wait_ack(1);
    let store =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    wait_until("group post admitted into its Feishu Session", || {
        store
            .list("local")
            .unwrap()
            .first()
            .is_some_and(|session| !session.run_ids.is_empty())
    });
    let sessions = store.list("local").unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.channel["conversation_kind"], "group");
    assert_eq!(session.channel["conversation_id"], GROUP_CHAT);
    assert_eq!(session.run_ids.len(), 1);
    let run = &session.run_ids[0];
    let completed = waiting(&host, &store, &session.id, run, 1);
    provider.assert_consumed();
    let turns = store.list_turns("local", &session.id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Completed);
    assert_eq!(turns[0].prompt.as_deref(), Some(POST_TEXT));

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let parts = requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .collect::<Vec<_>>();
    assert!(parts.iter().any(|part| {
        part["type"] == "text"
            && part["text"]
                .as_str()
                .is_some_and(|text| text.contains(POST_TEXT))
    }));
    let image_urls = parts
        .iter()
        .filter(|part| part["type"] == "image_url")
        .map(|part| part["image_url"]["url"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_urls =
        IMAGE_KEYS.map(|key| format!("data:image/png;base64,{}", STANDARD.encode(image(key))));
    assert_eq!(image_urls, expected_urls);

    let mounted = fs::read_to_string(
        host.base
            .workspace(run, "assistant")
            .join("post-images.txt"),
    )
    .unwrap();
    let paths = mounted.lines().map(PathBuf::from).collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0].file_name(), paths[1].file_name());
    assert!(paths.iter().all(|path| {
        path.starts_with("/in/channel") && path.extension().is_some_and(|ext| ext == "png")
    }));
    let feedback = goose::tool_feedback(&requests[1]);
    let result: Value =
        serde_json::from_str(feedback.last().unwrap()["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    let command_result = &result["output"][0]["value"];
    assert_eq!(command_result["exit_code"], 0, "{result}");
    let stdout = command_result["stdout"].as_str().unwrap();
    for key in IMAGE_KEYS {
        assert!(stdout.contains(&format!("{:x}", Sha256::digest(image(key)))));
    }
    assert!(stdout.ends_with("feishu-post-images-readonly"));

    let facts = platform.snapshot();
    assert_eq!(facts["downloads"].as_array().unwrap().len(), 2);
    for key in IMAGE_KEYS {
        assert!(facts["downloads"].as_array().unwrap().contains(&json!({
            "message_id":FIRST_MESSAGE,"file_key":key,"type":"image"
        })));
    }
    assert_eq!(facts["replies"].as_array().unwrap().len(), 1);
    assert_eq!(facts["replies"][0]["reply_to"], FIRST_MESSAGE);
    let card: Value =
        serde_json::from_str(facts["replies"][0]["body"]["content"].as_str().unwrap()).unwrap();
    assert_eq!(card["body"]["elements"][0]["content"], FIRST_REPLY);
    let confirmations = confirmed(&store, &session.id);
    assert_eq!(confirmations.len(), 1);
    assert_eq!(confirmations[0].data["kind"], "feishu_reply");
    assert_eq!(
        confirmations[0].data["content_sha256"],
        format!("{:x}", Sha256::digest(FIRST_REPLY.as_bytes()))
    );
    assert!(
        store
            .list_unfinished_channel_deliveries("local", &session.id)
            .unwrap()
            .is_empty()
    );
    host.evidence_report(
        &provider,
        "tests/goose_feishu/post.rs",
        json!({
            "group_post_text_and_two_native_images":true,"distinct_readonly_channel_mounts":true,
            "confirmed_original_group_reply":true,"durable_admission_ack":ack,
            "real_channel_deliveries":0
        }),
        json!({
            "gateway_binary_sha256":goose::digest(&gateway),"installed_plugin":installed,
            "binding":binding,"platform":facts,"run":completed,"turns":turns,
            "mounted_paths":paths,"confirmed_deliveries":confirmations,
            "native":host.native_fact(run,"assistant",1)
        }),
    );
}
