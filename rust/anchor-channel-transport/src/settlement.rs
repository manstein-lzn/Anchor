use crate::{
    GatewayError, WebhookClient,
    ledger::{Ledger, current_time_millis},
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch};
const MAX_IN_FLIGHT: usize = 32;

pub async fn settlement_loop(
    ledger: Arc<Ledger>,
    webhook: Arc<WebhookClient>,
    mut shutdown: watch::Receiver<bool>,
    errors: broadcast::Sender<GatewayError>,
) {
    let mut requests =
        FuturesUnordered::<BoxFuture<'static, (String, Result<(), GatewayError>)>>::new();
    let mut active = HashSet::new();
    let mut retry = tokio::time::interval(Duration::from_millis(100));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if *shutdown.borrow() {
            break;
        }
        let capacity = MAX_IN_FLIGHT.saturating_sub(active.len());
        if capacity > 0 {
            let limit = MAX_IN_FLIGHT + active.len();
            match ledger
                .blocking(move |ledger| ledger.pending_settlements(current_time_millis()?, limit))
                .await
            {
                Ok(settlements) => {
                    for settlement in settlements {
                        if active.contains(&settlement.receipt.key) {
                            continue;
                        }
                        let key = settlement.receipt.key.clone();
                        let request_webhook = webhook.clone();
                        active.insert(key.clone());
                        requests.push(
                            async move { (key, request_webhook.settle(&settlement).await) }.boxed(),
                        );
                        if active.len() >= MAX_IN_FLIGHT {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = errors.send(error);
                }
            }
        }
        tokio::select! {
            _ = stopping(&mut shutdown) => break,
            result = requests.next(), if !requests.is_empty() => {
                if let Some((key, result)) = result {
                    active.remove(&key);
                    let permanent = matches!(result, Err(GatewayError::Rejected));
                    let succeeded = result.is_ok();
                    // This await is outside the cancellable HTTP futures. The
                    // supervisor joins us before releasing the writer lease.
                    if let Err(error) = ledger.blocking(move |ledger| {
                        if permanent { ledger.abandon_settlement(&key) }
                        else { ledger.finish_settlement(&key, succeeded) }
                    }).await {
                        let _ = errors.send(error);
                    }
                    if !permanent && let Err(error) = result {
                        let _ = errors.send(error);
                    }
                }
            }
            _ = retry.tick() => {}
        }
    }
}

pub async fn stopping(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stopped| *stopped).await;
}
