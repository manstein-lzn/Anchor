use super::*;
use crate::application::ConversationSource;
use image::{DynamicImage, ImageBuffer, Rgb};
use tempfile::tempdir;

fn upload(name: &str, bytes: &[u8], media_type: Option<&str>) -> UploadedAttachment {
    UploadedAttachment {
        name: name.into(),
        data_base64: STANDARD.encode(bytes),
        media_type: media_type.map(str::to_owned),
    }
}

fn image_bytes(format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([32, 128, 224])))
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

fn metadata(root: &Path, run: &str, session: &str, prepared: &PreparedAttachments) -> RunMetadata {
    let mut metadata =
        RunMetadata::new(run.into(), "fixture".into(), "graph-digest".into(), root).unwrap();
    metadata.conversation = Some(ConversationSource {
        session: session.into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata.attachments = prepared.manifest();
    metadata
}

fn key(metadata: &RunMetadata) -> InvocationKey {
    InvocationKey {
        run_id: metadata.run_id.clone(),
        graph_digest: metadata.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    }
}

#[test]
fn upload_names_base64_and_mime_are_validated_before_freezing() {
    for name in [
        "",
        ".",
        "..",
        "/etc/passwd",
        "../file",
        "a/b",
        "a\\b",
        "a\n",
        "a\0",
    ] {
        assert!(prepare(&[upload(name, b"text", None)]).is_err(), "{name:?}");
    }
    assert!(prepare(&[upload(&"a".repeat(256), b"", None)]).is_err());
    assert!(prepare(&[upload("same", b"one", None), upload("same", b"two", None)]).is_err());
    let mut invalid = upload("valid.txt", b"text", None);
    invalid.data_base64 = "not base64!".into();
    assert!(prepare(&[invalid]).unwrap_err().contains("data_base64"));
    assert!(prepare(&[upload("valid.txt", b"text", Some("not a mime"))]).is_err());
    assert!(prepare(&[upload("image.png", b"plain text", Some("image/png"))]).is_err());
    let result = prepare(&[upload("unicode-\u{4e2d}.txt", b"", Some("text/plain"))]).unwrap();
    assert_eq!(result.manifest()[0].size, 0);
}

#[test]
fn file_count_and_decoded_byte_limits_apply_at_the_boundary() {
    let many = (0..17)
        .map(|index| upload(&format!("{index}.txt"), b"", None))
        .collect::<Vec<_>>();
    assert!(prepare(&many[..16]).is_ok());
    assert!(prepare(&many).unwrap_err().contains("16"));
    let mut bytes = vec![0; MAX_FILE_BYTES];
    let at_limit = prepare(&[upload("file.bin", &bytes, None)]).unwrap();
    assert_eq!(at_limit.manifest()[0].size, MAX_FILE_BYTES as u64);
    drop(at_limit);
    bytes.push(0);
    assert!(
        prepare(&[upload("file.bin", &bytes, None)])
            .unwrap_err()
            .contains("20 MiB")
    );
    let mut uploads = vec![
        upload("a", &bytes[..MAX_FILE_BYTES], None),
        upload("b", &bytes[..MAX_FILE_BYTES], None),
        upload("c", &bytes[..10 * MIB], None),
    ];
    assert!(prepare(&uploads).is_ok());
    uploads[2] = upload("c", &bytes[..10 * MIB + 1], None);
    assert!(prepare(&uploads).unwrap_err().contains("50 MiB"));
}

#[test]
fn supported_images_infer_mime_and_reject_declared_mismatches_and_corruption() {
    for (format, expected) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = image_bytes(format);
        let inferred = prepare(&[upload("picture", &bytes, None)]).unwrap();
        assert_eq!(inferred.manifest()[0].media_type.as_deref(), Some(expected));
        let parameterized = format!("{expected}; name=picture");
        let declared = prepare(&[upload("picture", &bytes, Some(&parameterized))]).unwrap();
        assert_eq!(inferred.manifest(), declared.manifest());
        for wrong in ["application/octet-stream", "image/gif"] {
            assert!(prepare(&[upload("picture", &bytes, Some(wrong))]).is_err());
        }
        assert!(prepare(&[upload("broken", &bytes[..12], None)]).is_err());
    }
    assert!(
        prepare(&[upload("animated.gif", b"GIF89a\x01\x00\x01\x00", None)])
            .unwrap_err()
            .contains("only PNG")
    );
}

#[test]
fn image_count_size_and_total_limits_apply_before_decode() {
    let bytes = image_bytes(ImageFormat::Png);
    let images = (0..9)
        .map(|index| upload(&format!("{index}.png"), &bytes, None))
        .collect::<Vec<_>>();
    assert!(prepare(&images[..8]).is_ok());
    assert!(prepare(&images).unwrap_err().contains("8 images"));
    let mut jpeg = image_bytes(ImageFormat::Jpeg);
    jpeg.resize(MAX_IMAGE_BYTES, 0);
    assert!(prepare(&[upload("a.jpg", &jpeg, None)]).is_ok());
    let uploads = [upload("a.jpg", &jpeg, None), upload("b.jpg", &jpeg, None)];
    assert!(prepare(&uploads).is_ok());
    let mut too_many = uploads.into_iter().collect::<Vec<_>>();
    too_many.push(upload("c.png", &bytes, None));
    assert!(prepare(&too_many).unwrap_err().contains("20 MiB total"));
    jpeg.push(0);
    assert!(
        prepare(&[upload("large.jpg", &jpeg, None)])
            .unwrap_err()
            .contains("10 MiB")
    );
}

#[test]
fn decoded_image_pixel_limit_rejects_oversized_headers() {
    let mut jpeg = image_bytes(ImageFormat::Jpeg);
    let frame = jpeg
        .windows(2)
        .position(|bytes| bytes == [0xff, 0xc0])
        .unwrap();
    jpeg[frame + 5..frame + 7].copy_from_slice(&4001_u16.to_be_bytes());
    jpeg[frame + 7..frame + 9].copy_from_slice(&5000_u16.to_be_bytes());
    assert!(
        prepare(&[upload("bomb.jpg", &jpeg, None)])
            .unwrap_err()
            .contains("20 million")
    );
}

fn animated_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[255, 0, 0]).unwrap();
    writer.write_image_data(&[0, 255, 0]).unwrap();
    writer.finish().unwrap();
    bytes
}

fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = kind.to_vec();
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        bytes.push(0);
    }
    bytes
}

fn animated_webp() -> Vec<u8> {
    let still = image_bytes(ImageFormat::WebP);
    let mut chunks = riff_chunk(b"VP8X", &[2, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
    chunks.extend(riff_chunk(b"ANIM", &[0; 6]));
    let mut frame = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 100, 0, 0, 2];
    frame.extend_from_slice(&still[12..]);
    chunks.extend(riff_chunk(b"ANMF", &frame));
    chunks.extend(riff_chunk(b"ANMF", &frame));
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(chunks.len() as u32 + 4).to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend(chunks);
    bytes
}

#[test]
fn animated_png_and_webp_are_rejected_using_decoder_metadata() {
    let png = animated_png();
    assert!(
        PngDecoder::new(Cursor::new(&png))
            .unwrap()
            .is_apng()
            .unwrap()
    );
    let webp = animated_webp();
    assert!(
        WebPDecoder::new(Cursor::new(&webp))
            .unwrap()
            .has_animation()
    );
    for bytes in [png, webp] {
        assert!(
            prepare(&[upload("animation", &bytes, None)])
                .unwrap_err()
                .contains("animated")
        );
    }
}

#[test]
fn frozen_inputs_preserve_upload_order_are_run_owned_and_bound_to_each_invocation() {
    let root = tempdir().unwrap();
    let png = image_bytes(ImageFormat::Png);
    let webp = image_bytes(ImageFormat::WebP);
    let prepared = prepare(&[
        upload("z.txt", b"private text", Some("text/plain")),
        upload("b.webp", &webp, None),
        upload("a.png", &png, None),
    ])
    .unwrap();
    let alice = metadata(root.path(), "alice-run", "alice", &prepared);
    freeze(root.path(), &alice, &prepared).unwrap();
    let inputs = node_inputs(root.path(), &alice, &key(&alice)).unwrap();
    assert_eq!(
        alice
            .attachments
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["z.txt", "b.webp", "a.png"]
    );
    assert_eq!(inputs.images.len(), 2);
    assert_eq!(inputs.images[0].bytes, webp);
    assert_eq!(inputs.images[0].media_type, "image/webp");
    assert_eq!(inputs.images[1].bytes, png);
    assert_eq!(inputs.images[1].media_type, "image/png");
    let reordered = prepare(&[
        upload("z.txt", b"private text", Some("text/plain")),
        upload("a.png", &png, None),
        upload("b.webp", &webp, None),
    ])
    .unwrap();
    assert!(freeze(root.path(), &alice, &reordered).is_err());
    let mount = inputs.mount.unwrap();
    assert_eq!(mount.destination, Path::new("/in/channel"));
    assert_eq!(
        fs::read(mount.source.join("z.txt")).unwrap(),
        b"private text"
    );
    let bob_inputs = prepare(&[upload("z.txt", b"different text", None)]).unwrap();
    let bob = metadata(root.path(), "bob-run", "bob", &bob_inputs);
    freeze(root.path(), &bob, &bob_inputs).unwrap();
    let bob_mount = node_inputs(root.path(), &bob, &key(&bob))
        .unwrap()
        .mount
        .unwrap();
    assert_ne!(mount.source, bob_mount.source);
    assert_eq!(
        fs::read(bob_mount.source.join("z.txt")).unwrap(),
        b"different text"
    );
    for wrong in [
        InvocationKey {
            run_id: bob.run_id.clone(),
            ..key(&alice)
        },
        InvocationKey {
            graph_digest: "other".into(),
            ..key(&alice)
        },
        InvocationKey {
            node_id: String::new(),
            ..key(&alice)
        },
        InvocationKey {
            invocation: 0,
            ..key(&alice)
        },
    ] {
        assert!(node_inputs(root.path(), &alice, &wrong).is_err());
    }
    let recovered: RunMetadata =
        serde_json::from_slice(&serde_json::to_vec(&alice).unwrap()).unwrap();
    verify(root.path(), &recovered).unwrap();
    assert_eq!(
        node_inputs(root.path(), &recovered, &key(&recovered))
            .unwrap()
            .images[1]
            .bytes,
        png
    );
}

#[test]
fn freeze_and_retry_never_rewrite_missing_or_tampered_inputs() {
    let root = tempdir().unwrap();
    let prepared = prepare(&[upload("message.txt", b"original", None)]).unwrap();
    let metadata = metadata(root.path(), "channel-run", "alice", &prepared);
    freeze(root.path(), &metadata, &prepared).unwrap();
    let directory = input_directory(root.path(), &metadata.run_id).unwrap();
    let path = directory.join("files/message.txt");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    freeze(root.path(), &metadata, &prepared).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    fs::write(&path, b"tampered").unwrap();
    assert!(
        verify(root.path(), &metadata)
            .unwrap_err()
            .contains("bytes changed")
    );
    assert!(node_inputs(root.path(), &metadata, &key(&metadata)).is_err());
    assert!(freeze(root.path(), &metadata, &prepared).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"tampered");
    fs::remove_file(&path).unwrap();
    assert!(freeze(root.path(), &metadata, &prepared).is_err());
    assert!(!path.exists());
    fs::remove_dir_all(&directory).unwrap();
    assert!(verify(root.path(), &metadata).is_err());
    assert!(node_inputs(root.path(), &metadata, &key(&metadata)).is_err());
}

#[test]
fn frozen_manifest_and_exact_file_set_are_checked() {
    let root = tempdir().unwrap();
    let prepared = prepare(&[upload("message.txt", b"original", None)]).unwrap();
    let metadata = metadata(root.path(), "channel-run", "alice", &prepared);
    freeze(root.path(), &metadata, &prepared).unwrap();
    let directory = input_directory(root.path(), &metadata.run_id).unwrap();
    fs::write(directory.join("files/extra.txt"), b"extra").unwrap();
    assert!(
        verify(root.path(), &metadata)
            .unwrap_err()
            .contains("file set")
    );
    fs::remove_file(directory.join("files/extra.txt")).unwrap();
    let manifest_path = directory.join("input.json");
    let mut manifest: FrozenInputs =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest.graph_digest = "other-graph".into();
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(
        verify(root.path(), &metadata)
            .unwrap_err()
            .contains("identity")
    );
}

#[cfg(unix)]
#[test]
fn host_paths_and_symlink_inputs_cannot_supply_attachment_bytes() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let prepared = prepare(&[upload("message.txt", b"original", None)]).unwrap();
    let metadata = metadata(root.path(), "channel-run", "alice", &prepared);
    freeze(root.path(), &metadata, &prepared).unwrap();
    let external = root.path().join("external.txt");
    fs::write(&external, b"original").unwrap();
    let path = input_directory(root.path(), &metadata.run_id)
        .unwrap()
        .join("files/message.txt");
    fs::remove_file(&path).unwrap();
    symlink(&external, &path).unwrap();
    assert!(
        verify(root.path(), &metadata)
            .unwrap_err()
            .contains("symlinks")
    );
    let alias = root.path().join("alias");
    symlink(root.path(), &alias).unwrap();
    assert!(freeze(&alias, &metadata, &prepared).is_err());
    assert!(remove(&alias, &metadata.run_id).is_err());
    for unsafe_id in ["../run", "/tmp/run", "a/b", "a\\b", ".", ".."] {
        assert!(input_directory(root.path(), unsafe_id).is_err());
    }
}

#[test]
fn removal_is_idempotent_and_no_attachment_metadata_stays_compatible() {
    let root = tempdir().unwrap();
    let prepared = prepare(&[upload("message.txt", b"original", None)]).unwrap();
    let attached = metadata(root.path(), "channel-run", "alice", &prepared);
    freeze(root.path(), &attached, &prepared).unwrap();
    remove(root.path(), &attached.run_id).unwrap();
    remove(root.path(), &attached.run_id).unwrap();
    assert!(
        !input_directory(root.path(), &attached.run_id)
            .unwrap()
            .exists()
    );
    let empty = prepare(&[]).unwrap();
    let old = metadata(root.path(), "old-run", "old", &empty);
    let mut encoded = serde_json::to_value(&old).unwrap();
    encoded.as_object_mut().unwrap().remove("attachments");
    let old: RunMetadata = serde_json::from_value(encoded).unwrap();
    freeze(root.path(), &old, &empty).unwrap();
    let inputs = node_inputs(root.path(), &old, &key(&old)).unwrap();
    assert!(inputs.images.is_empty());
    assert!(inputs.mount.is_none());
    assert!(!input_directory(root.path(), &old.run_id).unwrap().exists());
}
