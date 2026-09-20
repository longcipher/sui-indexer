use std::sync::Arc;

use tokio::sync::broadcast;

/// Live checkpoint feed for SSE streaming and status subscribers.
#[derive(Debug)]
pub struct BlockFeed {
    sender: broadcast::Sender<BlockUpdate>,
}

/// Checkpoint progress broadcast message.
#[derive(Debug, Clone, Copy)]
pub struct BlockUpdate {
    /// Newly committed checkpoint.
    pub committed: u64,
    /// Latest network tip observed.
    pub latest: u64,
}

impl BlockFeed {
    /// Create a feed with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Broadcast a newly committed checkpoint.
    pub fn broadcast(&self, committed: u64, latest: u64) {
        let _ = self.sender.send(BlockUpdate { committed, latest });
    }

    /// Subscribe to checkpoint updates.
    pub fn subscribe(&self) -> broadcast::Receiver<BlockUpdate> {
        self.sender.subscribe()
    }

    /// Share ownership for API handlers.
    pub fn shared(self: &Arc<Self>) -> Arc<Self> {
        Arc::clone(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_feed_broadcasts_to_subscribers() {
        let feed = BlockFeed::new(8);
        let mut receiver = feed.subscribe();
        feed.broadcast(10, 12);
        let update = receiver.try_recv().expect("update");
        assert_eq!(update.committed, 10);
        assert_eq!(update.latest, 12);
    }
}
