/*
 * OpenD's limit on history requests: at most 60 first pages in any 30 s. A request over it waits until the oldest
 * of the last 60 is 30 s old, instead of being refused by OpenD.
 */
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

pub struct Pace {
    max: usize,
    window: Duration,
    sent: Mutex<VecDeque<Instant>>,
}

impl Pace {
    pub fn new(max: usize, window: Duration) -> Self {
        Self { max: max.max(1), window, sent: Mutex::new(VecDeque::new()) }
    }

    /// OpenD's history limit.
    pub fn history() -> Self {
        Self::new(60, Duration::from_secs(30))
    }

    /// Waits for a free slot and takes it.
    pub async fn wait(&self) {
        loop {
            let wait = {
                let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                while sent.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
                    sent.pop_front();
                }
                if sent.len() < self.max {
                    sent.push_back(now);
                    return;
                }
                self.window - now.duration_since(sent[0])
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_61st_waits_until_the_first_is_30_s_old() {
        let pace = Pace::history();
        let start = Instant::now();
        for _ in 0..60 {
            pace.wait().await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        pace.wait().await;
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }
}
