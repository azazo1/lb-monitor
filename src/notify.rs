use anyhow::Result;

pub trait Notifier: Send + Sync {
    fn notify_update(&self, body: &str) -> Result<()>;
}

pub struct SystemNotifier;

pub struct NoopNotifier;

impl Notifier for SystemNotifier {
    fn notify_update(&self, body: &str) -> Result<()> {
        notify_rust::Notification::new()
            .summary("Leaderboard updated")
            .body(body)
            .show()?;
        Ok(())
    }
}

impl Notifier for NoopNotifier {
    fn notify_update(&self, _body: &str) -> Result<()> {
        Ok(())
    }
}
