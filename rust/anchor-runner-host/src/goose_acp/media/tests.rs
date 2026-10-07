use super::*;

#[test]
fn text_and_json_results_keep_the_existing_receipt_envelope() {
    let output = encode(vec![
        ToolResultContent::text("literal {text}"),
        ToolResultContent::json(json!({"value":7})),
    ])
    .unwrap();
    assert!(output.media.is_none());
    let value = json!({"ok":true,"output":output.observation,"anchor_receipt":"receipt"});
    assert_eq!(
        success(value.clone(), output.media).content,
        vec![ContentBlock::text(value.to_string())]
    );
}

#[cfg(not(feature = "legacy-regression"))]
fn image() -> ToolResultContent {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    ToolResultContent::image(STANDARD.encode(bytes.into_inner()), "image/png")
}

#[cfg(not(feature = "legacy-regression"))]
#[test]
fn image_payloads_are_native_mcp_blocks_and_anchor_facts_only_keep_a_digest() {
    let image = image();
    let ToolResultContent::Image { data, .. } = &image else {
        panic!("image fixture");
    };
    let output = encode(vec![
        ToolResultContent::text("before"),
        image.clone(),
        ToolResultContent::json(json!({"caption":"after"})),
    ])
    .unwrap();
    assert!(output.observation[1]["data"].is_null());
    assert_eq!(output.observation[1]["mime_type"], "image/png");
    assert_eq!(
        output.observation[1]["data_sha256"].as_str().unwrap().len(),
        64
    );
    let result = success(
        json!({"ok":true,"output":output.observation,"anchor_receipt":"receipt"}),
        output.media,
    );
    let header: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(header["anchor_receipt"], "receipt");
    assert_eq!(
        header["images"][0]["data_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(result.content[1], ContentBlock::text("before"));
    assert_eq!(result.content[2], ContentBlock::image(data, "image/png"));
    assert_eq!(
        result.content[3],
        ContentBlock::text("{\"caption\":\"after\"}")
    );
    assert!(!header.to_string().contains(data));
}

#[cfg(not(feature = "legacy-regression"))]
#[test]
fn malformed_or_overcount_images_do_not_leak_partial_content() {
    assert!(
        encode(vec![
            ToolResultContent::text("before"),
            ToolResultContent::image("AAEC", "image/png")
        ])
        .is_err()
    );
    assert!(encode(vec![image(); 9]).is_err());
}
