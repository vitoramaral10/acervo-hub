use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};

/// Agenda requisições de um tracker num único orçamento compartilhável.
///
/// Clones de um cliente Torznab carregam o mesmo `RateBudget`; buscas de
/// séries e filmes, portanto, não conseguem gastar em paralelo o limite do
/// mesmo tracker.
#[derive(Debug)]
pub struct RateBudget {
    interval: Duration,
    next: Mutex<Instant>,
}

impl RateBudget {
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Mutex::new(Instant::now()),
        }
    }

    pub(crate) async fn acquire(&self) {
        let slot = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let slot = (*next).max(now);
            *next = slot + self.interval;
            slot
        };

        sleep_until(slot).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn reservas_sequenciais_respeitam_o_intervalo() {
        let budget = RateBudget::new(Duration::from_secs(3));
        let start = Instant::now();

        budget.acquire().await;
        budget.acquire().await;
        budget.acquire().await;

        assert_eq!(Instant::now() - start, Duration::from_secs(6));
    }
}
