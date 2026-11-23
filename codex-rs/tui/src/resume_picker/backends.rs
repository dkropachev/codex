//! Configures app-server listing sources and owns each source's request and shutdown task.
//!
//! Each source retains its backend identity and filesystem policy. The separate source fan-out
//! coordinates their replies; this module keeps requests and accepted mutations on their owner.

use std::sync::Arc;

use super::BackgroundEvent;
use super::LoadedTranscript;
use super::PageCursor;
use super::PageLoadMode;
use super::PickerBackground;
use super::PickerLoadRequest;
use super::SessionTarget;
use super::load_app_server_page;
use super::load_transcript_preview;
use super::page_loading::PageCwdFilter;
use super::raw_reasoning_visibility;
use super::sources;
use super::thread_list_params;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::unread_revision;
use crate::legacy_core::config::Config;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::load_session_transcript;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

pub(crate) struct ResumePickerAppServers {
    ordered: Vec<AppServerSession>,
    displayed_owner: crate::backend_id::BackendId,
}

impl ResumePickerAppServers {
    pub(crate) fn new(
        ordered: Vec<AppServerSession>,
        displayed_owner: crate::backend_id::BackendId,
    ) -> Self {
        assert!(
            ordered
                .iter()
                .any(|session| session.backend_id() == displayed_owner)
        );
        Self {
            ordered,
            displayed_owner,
        }
    }

    pub(super) fn singleton(session: AppServerSession) -> Self {
        let displayed_owner = session.backend_id();
        Self::new(vec![session], displayed_owner)
    }
}

pub(super) struct AppServerPickerBackend {
    uses_remote_filesystem: bool,
    pub(super) session: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<Config>,
}

pub(super) fn resume_picker_backends(
    uses_remote_filesystem: bool,
    config: &Config,
    include_non_interactive: bool,
    app_servers: ResumePickerAppServers,
) -> (Vec<AppServerPickerBackend>, crate::backend_id::BackendId) {
    let displayed_owner = app_servers.displayed_owner;
    let backends = app_servers
        .ordered
        .into_iter()
        .map(|session| {
            let is_displayed_owner = session.backend_id() == displayed_owner;
            AppServerPickerBackend {
                uses_remote_filesystem: if is_displayed_owner {
                    uses_remote_filesystem
                } else {
                    session.uses_remote_workspace()
                },
                archive_request_handle: session.request_handle(),
                config: (!session.uses_remote_workspace()).then(|| config.clone()),
                session,
                include_non_interactive,
                raw_reasoning_visibility: raw_reasoning_visibility(config),
            }
        })
        .collect();
    (backends, displayed_owner)
}

pub(super) fn spawn_app_server_page_loader(
    uses_remote_filesystem: bool,
    app_server: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<Config>,
    output: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerBackground {
    spawn_app_server_page_loaders(
        vec![AppServerPickerBackend {
            uses_remote_filesystem,
            session: app_server,
            archive_request_handle,
            include_non_interactive,
            raw_reasoning_visibility,
            config,
        }],
        output,
    )
}

pub(super) fn spawn_app_server_page_loaders(
    backends: Vec<AppServerPickerBackend>,
    output: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerBackground {
    let mut sources = Vec::with_capacity(backends.len());
    for backend in backends {
        sources.push(spawn_app_server_source(backend));
    }
    sources::spawn(sources, output)
}

fn spawn_app_server_source(backend: AppServerPickerBackend) -> sources::Source {
    let AppServerPickerBackend {
        uses_remote_filesystem,
        session: app_server,
        archive_request_handle,
        include_non_interactive,
        raw_reasoning_visibility,
        config,
    } = backend;
    let backend_id = app_server.backend_id();
    let (bg_tx, events) = mpsc::unbounded_channel();
    let (request_tx, mut request_rx) = mpsc::unbounded_channel::<PickerLoadRequest>();

    let task = tokio::spawn(async move {
        let mut app_server = app_server;
        let mut page_cwd_filter = PageCwdFilter::default();
        while let Some(request) = request_rx.recv().await {
            match request {
                PickerLoadRequest::Page(request) => {
                    let mode = request.mode;
                    let provider_filter = request.provider_filter;
                    let cwd_filter = page_cwd_filter.for_request(
                        request.cursor.as_ref(),
                        request.cwd_filter.as_deref(),
                        request.filter_mode,
                        uses_remote_filesystem,
                    );
                    let cursor = request.cursor.map(|cursor| match cursor {
                        PageCursor::AppServer(cursor) => cursor,
                        PageCursor::Catalog | PageCursor::SourcesPending => {
                            unreachable!("catalog cursors never reach a source")
                        }
                    });
                    let params = thread_list_params(
                        cursor,
                        cwd_filter,
                        request.status,
                        provider_filter,
                        request.sort_key,
                        include_non_interactive,
                        matches!(request.mode, PageLoadMode::StateDbOnly),
                    );
                    let page = tokio::select! {
                        page = load_app_server_page(&mut app_server, params) => page,
                        _ = bg_tx.closed() => continue,
                    };
                    let _ = bg_tx.send(BackgroundEvent::Page {
                        request_token: request.request_token,
                        search_token: request.search_token,
                        mode,
                        page,
                    });
                }
                PickerLoadRequest::Acknowledge {
                    thread_id,
                    revision,
                    ..
                } => match app_server.acknowledge_read(thread_id, revision).await {
                    Ok(state) => {
                        let _ = bg_tx.send(BackgroundEvent::Acknowledged {
                            backend_id,
                            thread_id,
                            revision: unread_revision(Some(state)),
                        });
                    }
                    Err(error) => {
                        warn!(%thread_id, %error, "Could not acknowledge viewed transcript")
                    }
                },
                PickerLoadRequest::Preview { thread_id, .. } => {
                    let preview = tokio::select! {
                        preview = load_transcript_preview(&mut app_server, thread_id, config.as_ref()) => preview,
                        _ = bg_tx.closed() => continue,
                    };
                    let _ = bg_tx.send(BackgroundEvent::Preview { thread_id, preview });
                }
                PickerLoadRequest::Transcript {
                    thread_id,
                    cancellation,
                    ..
                } => {
                    tokio::select! {
                        transcript = load_session_transcript(
                            &mut app_server,
                            thread_id,
                            raw_reasoning_visibility,
                            config.as_ref(),
                        ) => {
                            let _ = bg_tx.send(BackgroundEvent::Transcript {
                                backend_id, thread_id,
                                transcript: transcript.map(|cells| LoadedTranscript {
                                    cells, unread_revision: app_server.unread_revisions.remove(&thread_id),
                                }),
                            });
                        }
                        _ = cancellation => {}
                        _ = bg_tx.closed() => {}
                    }
                }
                PickerLoadRequest::Archive { thread_id, .. } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadArchiveResponse>(ClientRequest::ThreadArchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-archive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadArchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|_| ())
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Archive { thread_id, result });
                }
                PickerLoadRequest::Unarchive { thread_id, .. } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadUnarchiveResponse>(ClientRequest::ThreadUnarchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-unarchive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadUnarchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|response| SessionTarget {
                            backend_id: app_server.backend_id(),
                            path: response.thread.path,
                            thread_id,
                            cwd: Some(response.thread.cwd.to_path_buf()),
                            history_mode: Some(response.thread.history_mode),
                        })
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Unarchive { thread_id, result });
                }
            }
        }
        if let Err(err) = app_server.shutdown().await {
            warn!(%err, "Failed to shut down app-server picker session");
        }
    });

    sources::Source {
        backend_id,
        loader: Arc::new(move |request: PickerLoadRequest| {
            let _ = request_tx.send(request);
        }),
        events,
        task,
    }
}
