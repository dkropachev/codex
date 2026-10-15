//! Exercises permit cleanup and progress without relying on I/O timing.

use std::future::pending;
use std::future::ready;
use std::io;
use std::io::Write;
use std::task::Poll;

use anyhow::Result;
use pretty_assertions::assert_eq;
use tokio::fs::File;
use tokio::sync::oneshot;

use super::FileHandleManager;
use super::FileReadBlock;
use super::MAX_OPEN_FILES;

/// A stalled open must let unrelated handles open and read without waiting for it.
#[tokio::test]
async fn stalled_open_does_not_block_other_handles() -> Result<()> {
    let manager = FileHandleManager::default();
    let mut stalled = Box::pin(manager.open("stalled".to_string(), pending()));
    assert!(futures::poll!(stalled.as_mut()).is_pending());

    let mut other = Box::pin(manager.open(
        "other".to_string(),
        ready(Ok(File::from_std(tempfile::tempfile()?))),
    ));
    let Poll::Ready(result) = futures::poll!(other.as_mut()) else {
        anyhow::bail!("an unrelated open was blocked by the stalled open");
    };
    assert_eq!(result?, "other");
    manager.read_block("other", /*offset*/ 0, /*len*/ 1).await?;
    Ok(())
}

/// Pending opens consume capacity before polling another file open.
#[tokio::test]
async fn pending_opens_reserve_capacity() -> Result<()> {
    let manager = FileHandleManager::default();
    let mut opens = Vec::new();
    for index in 0..MAX_OPEN_FILES {
        let mut open = Box::pin(manager.open(index.to_string(), pending()));
        assert!(futures::poll!(open.as_mut()).is_pending());
        opens.push(open);
    }
    let mut rejected = Box::pin(manager.open("overflow".to_string(), pending()));
    let Poll::Ready(Err(error)) = futures::poll!(rejected.as_mut()) else {
        anyhow::bail!("admission must fail before polling the file open");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    Ok(())
}

/// Failed and cancelled opens must return their permits for reuse.
#[test_case::test_case(true; "cancelled")]
#[test_case::test_case(false; "failed")]
#[tokio::test]
async fn unfinished_opens_release_capacity(cancelled: bool) -> Result<()> {
    let manager = FileHandleManager::default();
    for _ in 0..=MAX_OPEN_FILES {
        let mut open = Box::pin(manager.open("reused".to_string(), async {
            if cancelled {
                pending::<()>().await;
            }
            Err(io::Error::other("open failed"))
        }));
        let result = futures::poll!(open.as_mut());
        if cancelled {
            assert!(result.is_pending());
        } else {
            let Poll::Ready(Err(error)) = result else {
                anyhow::bail!("the open error must reach the caller");
            };
            assert_eq!(error.kind(), io::ErrorKind::Other);
        }
        drop(open);
    }
    Ok(())
}

/// Closing an entry frees capacity even while an operation retains its file descriptor.
#[tokio::test]
async fn close_releases_capacity_while_the_file_is_still_in_use() -> Result<()> {
    let manager = FileHandleManager::default();
    let file = tempfile::tempfile()?;
    for index in 0..MAX_OPEN_FILES {
        manager
            .open(
                index.to_string(),
                ready(Ok(File::from_std(file.try_clone()?))),
            )
            .await?;
    }
    let retained = manager.get("0")?;
    manager.close("0");
    manager
        .open("next".to_string(), ready(Ok(File::from_std(file))))
        .await?;
    drop(retained);
    Ok(())
}

/// A pending ID rejects replacement before its destructive opener is polled.
#[tokio::test]
async fn concurrent_opens_reject_duplicate_ids_without_touching_targets() -> Result<()> {
    let manager = FileHandleManager::default();
    let (send, receive) = oneshot::channel();
    let mut first = Box::pin(manager.open("shared".to_string(), async {
        receive.await.map_err(io::Error::other)
    }));
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert_eq!(
        manager.get("shared").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    let tmp = tempfile::tempdir()?;
    let existing = tmp.path().join("existing");
    let missing = tmp.path().join("missing");
    std::fs::write(&existing, b"original")?;
    for target in [&existing, &missing] {
        let error = manager
            .open("shared".to_string(), async {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(target)
                    .await
            })
            .await
            .expect_err("a pending ID must reject a duplicate");
        assert_eq!(
            (error.kind(), error.to_string()),
            (
                io::ErrorKind::InvalidInput,
                "file handle `shared` already exists".to_string()
            ),
        );
    }
    assert_eq!(std::fs::read(existing)?, b"original");
    assert!(!missing.exists());

    let mut file = tempfile::tempfile()?;
    file.write_all(b"first")?;
    send.send(File::from_std(file))
        .map_err(|_| anyhow::anyhow!("the pending open dropped its receiver"))?;
    assert_eq!(first.await?, "shared");
    assert_eq!(
        manager
            .read_block("shared", /*offset*/ 0, /*len*/ 6)
            .await?,
        FileReadBlock {
            bytes: b"first".to_vec(),
            eof: true
        },
    );
    Ok(())
}

/// Closing pending entries does not return capacity until their futures stop.
#[test_case::test_case(true; "close_all")]
#[test_case::test_case(false; "close")]
#[tokio::test]
async fn closed_pending_opens_keep_capacity_until_cancelled(close_all: bool) -> Result<()> {
    let manager = FileHandleManager::default();
    let mut opens = Vec::new();
    for index in 0..MAX_OPEN_FILES {
        let mut open = Box::pin(manager.open(index.to_string(), pending()));
        assert!(futures::poll!(open.as_mut()).is_pending());
        opens.push(open);
    }
    let duplicate = manager.open("0".to_string(), pending()).await.unwrap_err();
    assert_eq!(duplicate.to_string(), "file handle `0` already exists");
    if close_all {
        manager.close_all();
    } else {
        for index in 0..MAX_OPEN_FILES {
            manager.close(&index.to_string());
        }
    }
    let error = manager.open("0".to_string(), pending()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "at most 128 file handles may be open per connection"
    );
    drop(opens.pop());
    manager
        .open(
            "0".to_string(),
            ready(Ok(File::from_std(tempfile::tempfile()?))),
        )
        .await?;
    Ok(())
}

/// Stale completions and cancellation cannot remove or overwrite a reused reservation.
#[test_case::test_case(true, true; "close_all_cancel")]
#[test_case::test_case(true, false; "close_all_complete")]
#[test_case::test_case(false, true; "close_cancel")]
#[test_case::test_case(false, false; "close_complete")]
#[tokio::test]
async fn closed_pending_open_does_not_disturb_reused_id(
    close_all: bool,
    cancel: bool,
) -> Result<()> {
    let manager = FileHandleManager::default();
    let (old_send, old_receive) = oneshot::channel();
    let mut old = Box::pin(manager.open("shared".to_string(), async {
        old_receive.await.map_err(io::Error::other)
    }));
    assert!(futures::poll!(old.as_mut()).is_pending());
    if close_all {
        manager.close_all();
    } else {
        manager.close("shared");
    }
    let (new_send, new_receive) = oneshot::channel();
    let mut new = Box::pin(manager.open("shared".to_string(), async {
        new_receive.await.map_err(io::Error::other)
    }));
    assert!(futures::poll!(new.as_mut()).is_pending());
    if cancel {
        drop(old);
    } else {
        old_send
            .send(File::from_std(tempfile::tempfile()?))
            .map_err(|_| anyhow::anyhow!("old opener dropped its receiver"))?;
        assert_eq!(old.await.unwrap_err().kind(), io::ErrorKind::NotFound);
    }
    let duplicate = manager
        .open("shared".to_string(), pending())
        .await
        .unwrap_err();
    assert_eq!(duplicate.to_string(), "file handle `shared` already exists");
    let mut file = tempfile::tempfile()?;
    file.write_all(b"new")?;
    new_send
        .send(File::from_std(file))
        .map_err(|_| anyhow::anyhow!("new opener dropped its receiver"))?;
    assert_eq!(new.await?, "shared");
    assert_eq!(
        manager
            .read_block("shared", /*offset*/ 0, /*len*/ 4)
            .await?,
        FileReadBlock {
            bytes: b"new".to_vec(),
            eof: true
        },
    );
    Ok(())
}
