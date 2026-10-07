use std::{fs, os::unix::fs::symlink};

use anchor_docmost_tools::{
    Config, DEFAULT_ENDPOINT, MAX_UPLOAD_BYTES, UPLOAD_ROOT, UploadError, Uploader,
};

const PAGE_ID: &str = "a2c71876-bdc8-4d75-a4e3-42b83fa93c02";

fn uploader(root: &std::path::Path, key: &str) -> Uploader {
    Uploader::new(
        Config::new(key)
            .with_endpoint("http://127.0.0.1:9/upload")
            .expect("local-only endpoint")
            .with_upload_root(root)
            .expect("fixture root"),
    )
    .expect("uploader")
}

#[test]
fn default_config_is_fixed_and_endpoint_rejects_credentials_and_invalid_urls() {
    let config = Config::new("unused");
    assert_eq!(config.endpoint().as_str(), DEFAULT_ENDPOINT);
    assert_eq!(config.upload_root(), std::path::Path::new(UPLOAD_ROOT));
    for endpoint in [
        "",
        "/upload",
        "127.0.0.1/upload",
        "file:///tmp/image",
        "ftp://localhost/upload",
        "https://user:secret@localhost/upload",
        "http://user@localhost/upload",
        "http://@localhost/upload",
        "http://:secret@localhost/upload",
        "https://localhost/upload#secret",
        "https://",
        "http://localhost:invalid",
        " https://localhost/upload",
        "http://localhost/\nsecret",
    ] {
        assert!(matches!(
            Config::new("unused").with_endpoint(endpoint),
            Err(UploadError::Endpoint)
        ));
    }
    for endpoint in [
        "http://127.0.0.1:3210/upload",
        "https://docmost.example/api/files/upload",
        "http://[::1]:3210/upload?deployment=fixture",
    ] {
        assert!(Config::new("unused").with_endpoint(endpoint).is_ok());
    }
}

#[test]
fn injected_root_must_be_an_absolute_existing_directory_without_symlinks() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("assets");
    fs::create_dir(&root).expect("root");
    let alias = parent.path().join("alias");
    symlink(&root, &alias).expect("root symlink");
    let file = parent.path().join("file");
    fs::write(&file, b"file").expect("file");
    for path in [
        root.join(".."),
        parent.path().join("missing"),
        alias.clone(),
        alias.join("child"),
        file,
        std::path::PathBuf::from("relative"),
    ] {
        assert!(matches!(
            Config::new("unused").with_upload_root(path),
            Err(UploadError::Path)
        ));
    }
    assert!(Config::new("unused").with_upload_root(root).is_ok());
}

#[tokio::test]
async fn paths_reject_escape_all_symlinks_directories_fifo_and_socket_without_http() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("assets");
    let sibling = parent.path().join("assets-other");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&sibling).expect("sibling");
    let good = root.join("good.png");
    let outside = parent.path().join("outside.png");
    fs::write(&good, b"good").expect("good image");
    fs::write(&outside, b"private outside bytes").expect("outside");
    fs::write(sibling.join("image.png"), b"outside").expect("sibling image");
    fs::create_dir(root.join("directory.png")).expect("directory");
    symlink(&good, root.join("inside.png")).expect("inside symlink");
    symlink(&outside, root.join("outside.png")).expect("outside symlink");
    symlink(&root, root.join("alias")).expect("directory symlink");
    symlink(root.join("missing.png"), root.join("dangling.png")).expect("dangling symlink");
    let fifo = root.join("fifo.png");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .expect("FIFO");
    let socket = root.join("socket.png");
    let _socket = std::os::unix::net::UnixListener::bind(&socket).expect("Unix socket");
    let uploader = uploader(&root, "not-a-production-key");
    let cases = vec![
        "relative.png".to_owned(),
        outside.to_str().expect("outside").to_owned(),
        sibling
            .join("image.png")
            .to_str()
            .expect("sibling")
            .to_owned(),
        root.join("../outside.png")
            .to_str()
            .expect("escape")
            .to_owned(),
        root.join("nested/../../outside.png")
            .to_str()
            .expect("escape")
            .to_owned(),
        root.join("inside.png").to_str().expect("inside").to_owned(),
        root.join("outside.png")
            .to_str()
            .expect("outside")
            .to_owned(),
        root.join("alias/good.png")
            .to_str()
            .expect("alias")
            .to_owned(),
        root.join("dangling.png")
            .to_str()
            .expect("dangling")
            .to_owned(),
        root.join("directory.png")
            .to_str()
            .expect("directory")
            .to_owned(),
        root.join("missing.png")
            .to_str()
            .expect("missing")
            .to_owned(),
        root.to_str().expect("root").to_owned(),
        fifo.to_str().expect("FIFO").to_owned(),
        socket.to_str().expect("socket").to_owned(),
        "/dev/null".to_owned(),
        "/etc/passwd".to_owned(),
        "/proc/self/environ".to_owned(),
        format!("{}/image\n.png", root.display()),
        format!("{}/bad\".png", root.display()),
    ];
    for path in cases {
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            uploader.upload(&path, PAGE_ID, None),
        )
        .await
        .expect("special files must not block")
        .expect_err("invalid path");
        assert_eq!(error, UploadError::Path, "{path}");
    }
}

#[tokio::test]
async fn uuid_mime_size_and_missing_key_fail_before_transport() {
    let root = tempfile::tempdir().expect("root");
    let good = root.path().join("good.png");
    fs::write(&good, b"image").expect("image");
    let uploader = uploader(root.path(), "");
    for page in [
        "",
        "not-a-uuid",
        "1234",
        "../../secret",
        "a2c71876-bdc8-4d75-a4e3-42b83fa93c0z",
    ] {
        assert_eq!(
            uploader
                .upload(good.to_str().expect("path"), page, None)
                .await
                .expect_err("bad page UUID"),
            UploadError::Uuid
        );
    }
    for existing in [
        "",
        "bad",
        "fixture-secret",
        "a2c71876-bdc8-4d75-a4e3-42b83fa93c0z",
    ] {
        assert_eq!(
            uploader
                .upload(good.to_str().expect("path"), PAGE_ID, Some(existing))
                .await
                .expect_err("bad attachment UUID"),
            UploadError::Uuid
        );
    }
    for name in [
        "image.gif",
        "image.txt",
        "image",
        "image.svg.exe",
        "image.avif",
    ] {
        let path = root.path().join(name);
        fs::write(&path, b"bytes").expect("unsupported image");
        assert_eq!(
            uploader
                .upload(path.to_str().expect("path"), PAGE_ID, None)
                .await
                .expect_err("unsupported MIME"),
            UploadError::Mime
        );
    }
    for (name, size) in [("empty.png", 0), ("large.svg", MAX_UPLOAD_BYTES as u64 + 1)] {
        let path = root.path().join(name);
        fs::File::create(&path)
            .expect("image")
            .set_len(size)
            .expect("image length");
        assert_eq!(
            uploader
                .upload(path.to_str().expect("path"), PAGE_ID, None)
                .await
                .expect_err("invalid size"),
            UploadError::Size
        );
    }
    for page in [
        PAGE_ID,
        "a2c71876bdc84d75a4e342b83fa93c02",
        "{a2c71876-bdc8-4d75-a4e3-42b83fa93c02}",
        "urn:uuid:a2c71876-bdc8-4d75-a4e3-42b83fa93c02",
    ] {
        assert_eq!(
            uploader
                .upload(good.to_str().expect("path"), page, None)
                .await
                .expect_err("missing key"),
            UploadError::MissingKey
        );
    }
    let error = match Uploader::new(Config::new("fixture-secret\r\nAuthorization: leaked")) {
        Ok(_) => panic!("invalid authorization must fail"),
        Err(error) => error,
    };
    assert_eq!(error, UploadError::InvalidKey);
    assert!(!error.to_string().contains("fixture-secret"));
}
