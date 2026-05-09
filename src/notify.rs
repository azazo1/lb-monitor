use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::config::{MailConfig, SmtpSecurity};

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify_update(&self, subject: &str, body: &str) -> Result<()>;
}

pub struct MailNotifier {
    mailer: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    to: Vec<Mailbox>,
}

pub struct NoopNotifier;

impl MailNotifier {
    pub fn new(config: &MailConfig) -> Result<Self> {
        let span = tracing::info_span!("mail_notifier_new");
        let _entered = span.enter();
        let smtp = &config.smtp;
        if smtp.host.is_empty() {
            return Err(anyhow!("smtp host is required when mail is enabled"));
        }
        let from = smtp
            .from
            .as_ref()
            .ok_or_else(|| anyhow!("smtp from is required when mail is enabled"))?
            .parse::<Mailbox>()
            .context("failed to parse smtp from address")?;
        let to = smtp
            .to
            .iter()
            .map(|value| {
                value
                    .parse::<Mailbox>()
                    .context("failed to parse smtp recipient")
            })
            .collect::<Result<Vec<_>>>()?;
        if to.is_empty() {
            return Err(anyhow!("at least one smtp recipient is required"));
        }

        let mut builder = match smtp.security {
            SmtpSecurity::Plain => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host)
            }
            SmtpSecurity::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(
                &smtp.host,
            )
            .with_context(|| format!("failed to build starttls transport for {}", smtp.host))?,
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
                .with_context(|| format!("failed to build tls transport for {}", smtp.host))?,
        }
        .port(smtp.port);

        if let (Some(username), Some(password)) = (&smtp.username, &smtp.password) {
            builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
        }

        Ok(Self {
            mailer: builder.build(),
            from,
            to,
        })
    }

    fn build_message(&self, subject: &str, body: &str) -> Result<Message> {
        let span = tracing::info_span!("build_mail_message", subject = %subject);
        let _entered = span.enter();
        let mut builder = Message::builder().from(self.from.clone()).subject(subject);
        for recipient in &self.to {
            builder = builder.to(recipient.clone());
        }
        builder
            .body(body.to_string())
            .context("failed to build mail message")
    }
}

#[async_trait]
impl Notifier for MailNotifier {
    async fn notify_update(&self, subject: &str, body: &str) -> Result<()> {
        let span = tracing::info_span!("notify_update", subject = %subject);
        let _entered = span.enter();
        let message = self.build_message(subject, body)?;
        self.mailer
            .send(message)
            .await
            .context("failed to send mail notification")?;
        Ok(())
    }
}

#[async_trait]
impl Notifier for NoopNotifier {
    async fn notify_update(&self, _subject: &str, _body: &str) -> Result<()> {
        let span = tracing::info_span!("noop_notify_update");
        let _entered = span.enter();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SmtpConfig, SmtpSecurity};

    #[tokio::test]
    async fn builds_mail_for_multiple_recipients() {
        let notifier = MailNotifier::new(&MailConfig {
            enabled: true,
            smtp: SmtpConfig {
                host: "smtp.example.com".to_string(),
                port: 25,
                username: None,
                password: None,
                from: Some("sender@example.com".to_string()),
                to: vec![
                    "alpha@example.com".to_string(),
                    "beta@example.com".to_string(),
                ],
                security: SmtpSecurity::Plain,
            },
        })
        .expect("build notifier");

        let message = notifier
            .build_message("Leaderboard updated", "body")
            .expect("build message");
        let formatted = String::from_utf8(message.formatted()).expect("utf8");

        assert!(formatted.contains("alpha@example.com"));
        assert!(formatted.contains("beta@example.com"));
        drop(notifier);
    }
}
