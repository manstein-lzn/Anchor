use super::*;

fn publication_fixture(replacing: bool) -> (tempfile::TempDir, File, File) {
    let temporary = tempfile::tempdir().unwrap();
    let (_, staged_parent) = directory(&temporary.path().join("staged"), true).unwrap();
    let (_, plugins) = directory(&temporary.path().join("plugins"), true).unwrap();
    fs::create_dir(temporary.path().join("staged/demo")).unwrap();
    fs::write(temporary.path().join("staged/demo/value"), "new").unwrap();
    if replacing {
        fs::create_dir(temporary.path().join("plugins/demo")).unwrap();
        fs::write(temporary.path().join("plugins/demo/value"), "old").unwrap();
    }
    (temporary, staged_parent, plugins)
}

#[test]
fn replacement_exchange_keeps_previous_tree_until_publication_is_durable() {
    let (temporary, staged_parent, plugins) = publication_fixture(true);
    publish(
        &staged_parent,
        &plugins,
        "demo",
        true,
        |staged, installed| {
            assert_eq!(
                fs::read(temporary.path().join("plugins/demo/value")).unwrap(),
                b"new"
            );
            assert_eq!(
                fs::read(temporary.path().join("staged/demo/value")).unwrap(),
                b"old"
            );
            sync_publication(staged, installed)
        },
    )
    .unwrap();
}

#[test]
fn failed_publication_sync_rolls_back_both_new_and_replaced_installs() {
    for replacing in [false, true] {
        let (temporary, staged_parent, plugins) = publication_fixture(replacing);
        let mut attempts = 0;
        let error = publish(
            &staged_parent,
            &plugins,
            "demo",
            replacing,
            |staged, installed| {
                attempts += 1;
                if attempts == 1 {
                    return Err(io::Error::other("fixture fsync fault").into());
                }
                sync_publication(staged, installed)
            },
        )
        .unwrap_err();
        assert!(matches!(error, InstallError::Io(_)));
        assert_eq!(attempts, 2);
        assert_eq!(
            fs::read(temporary.path().join("staged/demo/value")).unwrap(),
            b"new"
        );
        if replacing {
            assert_eq!(
                fs::read(temporary.path().join("plugins/demo/value")).unwrap(),
                b"old"
            );
        } else {
            assert!(!temporary.path().join("plugins/demo").exists());
        }
    }
}

#[test]
fn rollback_durability_failure_is_explicitly_uncertain() {
    let (temporary, staged_parent, plugins) = publication_fixture(true);
    let error = publish(&staged_parent, &plugins, "demo", true, |_, _| {
        Err(io::Error::other("fixture fsync fault").into())
    })
    .unwrap_err();
    assert!(matches!(error, InstallError::PublicationUncertain));
    assert_eq!(
        fs::read(temporary.path().join("plugins/demo/value")).unwrap(),
        b"old"
    );
    assert_eq!(
        fs::read(temporary.path().join("staged/demo/value")).unwrap(),
        b"new"
    );
}

#[test]
fn publication_never_clobbers_a_concurrently_created_destination() {
    let (temporary, staged_parent, plugins) = publication_fixture(true);
    let error = publish(&staged_parent, &plugins, "demo", false, sync_publication).unwrap_err();
    assert!(matches!(error, InstallError::AlreadyExists));
    assert_eq!(
        fs::read(temporary.path().join("plugins/demo/value")).unwrap(),
        b"old"
    );
}

#[test]
fn failed_exchange_does_not_move_the_previous_install() {
    let (temporary, staged_parent, plugins) = publication_fixture(true);
    fs::remove_dir_all(temporary.path().join("staged/demo")).unwrap();
    assert!(publish(&staged_parent, &plugins, "demo", true, sync_publication).is_err());
    assert_eq!(
        fs::read(temporary.path().join("plugins/demo/value")).unwrap(),
        b"old"
    );
}
