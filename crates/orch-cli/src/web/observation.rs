//! The web parent observes liveness only. Entity rendering belongs to the PTY's TUI.
use super::{DaemonDown, PREFLIGHT_TIMEOUT};
use crate::tui::Connector;
use orch_client::{Client, ClientError, Subscription, SubscriptionEvent};
use orch_proto::{bounded::Id, error::NoDaemonDetail, frame::Store, ops::args::SubscribeArgs};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub(super) struct Observation {
    pub(super) status: watch::Receiver<Result<Id, DaemonDown>>,
    task: JoinHandle<()>,
}

impl Observation {
    pub(super) async fn start(
        client: Client,
        connector: Connector,
        shutdown: CancellationToken,
    ) -> Result<Self, ClientError> {
        let subscription = subscribe(client).await?;
        let (status, receiver) = watch::channel(Ok(subscription.instance().clone()));
        let task = tokio::spawn(observe(subscription, connector, shutdown, status));
        Ok(Self {
            status: receiver,
            task,
        })
    }

    pub(super) async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn subscribe(client: Client) -> Result<Subscription, ClientError> {
    client
        .into_subscription(SubscribeArgs {
            // One low-volume store is enough to keep the session open. We discard
            // its changes: no browser/parent cache and no domain event relay.
            stores: orch_proto::bounded::BoundedVec::new(vec![Store::State])
                .expect("one store is within the protocol bound"),
            filter: None,
            bus: false,
        })
        .await
}

async fn observe(
    mut subscription: Subscription,
    connector: Connector,
    shutdown: CancellationToken,
    status: watch::Sender<Result<Id, DaemonDown>>,
) {
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            event = subscription.next() => match event {
                Ok(SubscriptionEvent::Change { .. } | SubscriptionEvent::Bus { .. }) => {}
                Ok(SubscriptionEvent::ResyncRequired { .. }) => {
                    let _ = status.send_replace(Err(down(&connector, NoDaemonDetail::ConnectionLost)));
                    // A gap ends the handle. Release it BEFORE opening its
                    // replacement; never accumulate observation sessions.
                    drop(subscription);
                    let reconnect = async { subscribe(connector.connect().await?).await };
                    let result = tokio::select! {
                        biased;
                        () = shutdown.cancelled() => return,
                        result = tokio::time::timeout(PREFLIGHT_TIMEOUT, reconnect) => result,
                    };
                    match result {
                        Ok(Ok(fresh)) => {
                            let _ = status.send_replace(Ok(fresh.instance().clone()));
                            subscription = fresh;
                        }
                        Ok(Err(error)) => {
                            let _ = status.send_replace(Err(client_down(&connector, error)));
                            return;
                        }
                        Err(_) => {
                            let _ = status.send_replace(Err(down(&connector, NoDaemonDetail::Timeout)));
                            return;
                        }
                    }
                }
                Err(_) => {
                    let _ = status.send_replace(Err(down(&connector, NoDaemonDetail::ConnectionLost)));
                    // EOF/typed errors are an outage, not an automatic retry
                    // loop. A later authenticated page/WS retry opens anew.
                    return;
                }
            }
        }
    }
}

pub(super) fn client_down(connector: &Connector, error: ClientError) -> DaemonDown {
    DaemonDown {
        message: error.render(connector.agent_context()),
        exit_code: error.exit_code(),
    }
}

pub(super) fn down(connector: &Connector, detail: NoDaemonDetail) -> DaemonDown {
    DaemonDown {
        message: orch_client::NoDaemon::no_daemon(
            &connector.project_root().to_string_lossy(),
            detail,
        )
        .render(connector.agent_context()),
        exit_code: 7,
    }
}
