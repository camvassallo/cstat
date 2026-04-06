use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

/// Token-bucket rate limiter for NatStat API.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

#[derive(Debug)]
struct RateLimiterInner {
    max_tokens: u32,
    tokens: u32,
    refill_interval: Duration,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a rate limiter with the given max calls per hour.
    pub fn new(max_per_hour: u32) -> Self {
        // Refill one token every (3600 / max_per_hour) seconds.
        let refill_interval = Duration::from_secs_f64(3600.0 / max_per_hour as f64);
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                max_tokens: max_per_hour,
                tokens: max_per_hour,
                refill_interval,
                last_refill: Instant::now(),
            })),
        }
    }

    /// Wait until a token is available, then consume it.
    pub async fn acquire(&self) {
        loop {
            let wait_duration = {
                let mut inner = self.inner.lock().await;
                inner.refill();
                if inner.tokens > 0 {
                    inner.tokens -= 1;
                    return;
                }
                inner.refill_interval
            };
            tokio::time::sleep(wait_duration).await;
        }
    }

    /// Check how many tokens are currently available (for diagnostics).
    pub async fn available(&self) -> u32 {
        let mut inner = self.inner.lock().await;
        inner.refill();
        inner.tokens
    }
}

impl RateLimiterInner {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let new_tokens = (elapsed.as_secs_f64() / self.refill_interval.as_secs_f64()) as u32;
        if new_tokens > 0 {
            self.tokens = (self.tokens + new_tokens).min(self.max_tokens);
            self.last_refill = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_starts_full() {
        let limiter = RateLimiter::new(500);
        assert_eq!(limiter.available().await, 500);
    }

    #[tokio::test]
    async fn test_rate_limiter_consumes_tokens() {
        let limiter = RateLimiter::new(500);
        limiter.acquire().await;
        limiter.acquire().await;
        assert_eq!(limiter.available().await, 498);
    }

    #[tokio::test]
    async fn test_rate_limiter_refills() {
        // Use a high rate so refill happens quickly.
        let limiter = RateLimiter::new(360_000); // 100/sec
        limiter.acquire().await;
        let before = limiter.available().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let after = limiter.available().await;
        assert!(after >= before, "tokens should refill over time");
    }
}
