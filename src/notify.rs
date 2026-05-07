use anyhow::{Context, Result, anyhow};
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};

use crate::config::{MailConfig, SmtpSecurity};

pub trait Notifier: Send + Sync {
    fn notify_update(&self, subject: &str, body: &str) -> Result<()>;
}

pub struct MailNotifier {
    mailer: SmtpTransport,
    from: Mailbox,
    to: Vec<Mailbox>,
}

pub struct NoopNotifier;

impl MailNotifier {
    pub fn new(config: &MailConfig) -> Result<Self> {
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
            SmtpSecurity::Plain => SmtpTransport::builder_dangerous(&smtp.host),
            SmtpSecurity::StartTls => SmtpTransport::starttls_relay(&smtp.host)
                .with_context(|| format!("failed to build starttls transport for {}", smtp.host))?,
            SmtpSecurity::Tls => SmtpTransport::relay(&smtp.host)
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
        let mut builder = Message::builder().from(self.from.clone()).subject(subject);
        for recipient in &self.to {
            builder = builder.to(recipient.clone());
        }
        builder
            .body(body.to_string())
            .context("failed to build mail message")
    }
}

impl Notifier for MailNotifier {
    fn notify_update(&self, subject: &str, body: &str) -> Result<()> {
        let message = self.build_message(subject, body)?;
        self.mailer
            .send(&message)
            .context("failed to send mail notification")?;
        Ok(())
    }
}

impl Notifier for NoopNotifier {
    fn notify_update(&self, _subject: &str, _body: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SmtpConfig, SmtpSecurity};

    #[test]
    fn builds_mail_for_multiple_recipients() {
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
    }
}
