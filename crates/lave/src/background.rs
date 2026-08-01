//! Asking the desktop for permission to keep running with no window.
//!
//! Outside a sandbox the portal is often absent entirely; that is not a refusal, and
//! the application carries on. An explicit refusal is respected.

use ashpd::desktop::background::Background;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundOutcome {
    Granted,
    Denied,
    /// No portal on this system, so there is nothing to ask.
    Unavailable,
}

pub async fn request() -> BackgroundOutcome {
    let request = Background::request()
        .reason("Lave Station keeps watching Docker while its window is closed.")
        .auto_start(false)
        .send()
        .await;

    let request = match request {
        Ok(request) => request,
        Err(error) => {
            tracing::info!("no background portal available: {error}");
            return BackgroundOutcome::Unavailable;
        }
    };

    match request.response() {
        Ok(response) if response.run_in_background() => BackgroundOutcome::Granted,
        Ok(_) => BackgroundOutcome::Denied,
        Err(error) => {
            tracing::info!("the background portal did not answer: {error}");
            BackgroundOutcome::Unavailable
        }
    }
}
