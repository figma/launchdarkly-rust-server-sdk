use crate::{LAUNCHDARKLY_EVENT_SCHEMA_HEADER, LAUNCHDARKLY_PAYLOAD_ID_HEADER};
use crossbeam_channel::Sender;
use std::collections::HashMap;

use crate::ureq::{is_http_error_recoverable, is_http_success};
use chrono::DateTime;
#[cfg(test)]
use http_types::StatusCode;
use ureq;
use url::Url;
use uuid::Uuid;

use super::event::OutputEvent;

pub struct EventSenderResult {
    pub(super) time_from_server: u128,
    pub(super) success: bool,
    pub(super) must_shutdown: bool,
}

pub trait EventSender: Send + Sync {
    fn send_event_data(&self, events: Vec<OutputEvent>, result_tx: Sender<EventSenderResult>);
}

#[derive(Clone)]
pub struct ReqwestEventSender {
    url: Url,
    sdk_key: String,
    default_headers: HashMap<String, String>,
}

impl ReqwestEventSender {
    pub fn new(default_headers: HashMap<String, String>, url: Url, sdk_key: &str) -> Self {
        Self {
            default_headers,
            url,
            sdk_key: sdk_key.to_owned(),
        }
    }

    fn get_server_time_from_response(&self, response: &ureq::Response) -> u128 {
        let date_value = response.header("date").unwrap_or("");

        match DateTime::parse_from_rfc2822(&date_value) {
            Ok(date) => date.timestamp_millis() as u128,
            Err(_) => 0,
        }
    }
}

impl EventSender for ReqwestEventSender {
    fn send_event_data(&self, events: Vec<OutputEvent>, result_tx: Sender<EventSenderResult>) {
        let uuid = Uuid::new_v4();

        debug!(
            "Sending ({}): {}",
            uuid,
            serde_json::to_string_pretty(&events).unwrap_or_else(|e| e.to_string())
        );

        let json = match serde_json::to_vec(&events) {
            Ok(json) => json,
            Err(e) => {
                error!(
                    "Failed to serialize event payload. Some events were dropped: {:?}",
                    e
                );
                return;
            }
        };

        for _ in 0..2 {
            let mut request = ureq::request_url("POST", &self.url)
                .set("Content-Type", "application/json")
                .set("Authorization", &self.sdk_key.clone())
                .set("User-Agent", &*crate::USER_AGENT)
                .set(
                    LAUNCHDARKLY_EVENT_SCHEMA_HEADER,
                    crate::CURRENT_EVENT_SCHEMA,
                )
                .set(LAUNCHDARKLY_PAYLOAD_ID_HEADER, &uuid.to_string());

            for (k, v) in self.default_headers.clone() {
                request = request.set(&k, &v);
            }

            let response = match request.send_json(json.clone()) {
                Ok(response) => response,
                Err(ureq::Error::Status(_code, response)) => response,
                Err(ureq::Error::Transport(_transport)) => {
                    return;
                }
            };

            debug!("sent event: {:?}", response);

            if is_http_success(response.status()) {
                let _ = result_tx.send(EventSenderResult {
                    success: true,
                    time_from_server: self.get_server_time_from_response(&response),
                    must_shutdown: false,
                });
                return;
            }

            if !is_http_error_recoverable(response.status()) {
                let _ = result_tx.send(EventSenderResult {
                    success: false,
                    time_from_server: 0,
                    must_shutdown: true,
                });
                return;
            }
        }

        let _ = result_tx.send(EventSenderResult {
            success: false,
            time_from_server: 0,
            must_shutdown: false,
        });
    }
}

#[cfg(test)]
pub(crate) struct InMemoryEventSender {
    event_tx: Sender<OutputEvent>,
}

#[cfg(test)]
impl InMemoryEventSender {
    pub(crate) fn new(event_tx: Sender<OutputEvent>) -> Self {
        Self { event_tx }
    }
}

#[cfg(test)]
impl EventSender for InMemoryEventSender {
    fn send_event_data(&self, events: Vec<OutputEvent>, sender: Sender<EventSenderResult>) {
        events
            .into_iter()
            .for_each(|event| self.event_tx.send(event).expect("event send failed"));
        sender
            .send(EventSenderResult {
                time_from_server: 0,
                success: true,
                must_shutdown: true,
            })
            .expect("result send failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use mockito::mock;
    use test_case::test_case;

    #[test_case(StatusCode::Continue, true)]
    #[test_case(StatusCode::Ok, true)]
    #[test_case(StatusCode::MultipleChoice, true)]
    #[test_case(StatusCode::BadRequest, true)]
    #[test_case(StatusCode::Unauthorized, false)]
    #[test_case(StatusCode::RequestTimeout, true)]
    #[test_case(StatusCode::Conflict, false)]
    #[test_case(StatusCode::TooManyRequests, true)]
    #[test_case(StatusCode::RequestHeaderFieldsTooLarge, false)]
    #[test_case(StatusCode::InternalServerError, true)]
    fn can_determine_recoverable_errors(status: StatusCode, is_recoverable: bool) {
        assert_eq!(
            is_recoverable,
            is_http_error_recoverable(status.to_string().parse::<u16>().unwrap())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn can_parse_server_time_from_response() {
        let _mock = mock("POST", "/bulk")
            .with_status(200)
            .with_header("date", "Fri, 13 Feb 2009 23:31:30 GMT")
            .create();

        let (tx, rx) = bounded::<EventSenderResult>(5);
        let event_sender = build_event_sender();

        event_sender.send_event_data(vec![], tx);

        let sender_result = rx.recv().expect("Failed to receive sender_result");
        assert!(sender_result.success);
        assert!(!sender_result.must_shutdown);
        assert_eq!(sender_result.time_from_server, 1234567890000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unrecoverable_failure_requires_shutdown() {
        let _mock = mock("POST", "/bulk").with_status(401).create();

        let (tx, rx) = bounded::<EventSenderResult>(5);
        let event_sender = build_event_sender();

        event_sender.send_event_data(vec![], tx);

        let sender_result = rx.recv().expect("Failed to receive sender_result");
        assert!(!sender_result.success);
        assert!(sender_result.must_shutdown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recoverable_failures_are_attempted_multiple_times() {
        let mock = mock("POST", "/bulk").with_status(400).expect(2).create();

        let (tx, rx) = bounded::<EventSenderResult>(5);
        let event_sender = build_event_sender();

        event_sender.send_event_data(vec![], tx);

        let sender_result = rx.recv().expect("Failed to receive sender_result");
        assert!(!sender_result.success);
        assert!(!sender_result.must_shutdown);
        mock.assert();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retrying_requests_can_eventually_succeed() {
        let _failed = mock("POST", "/bulk").with_status(400).create();
        let _succeed = mock("POST", "/bulk")
            .with_status(200)
            .with_header("date", "Fri, 13 Feb 2009 23:31:30 GMT")
            .create();

        let (tx, rx) = bounded::<EventSenderResult>(5);
        let event_sender = build_event_sender();

        event_sender.send_event_data(vec![], tx);

        let sender_result = rx.recv().expect("Failed to receive sender_result");
        assert!(sender_result.success);
        assert!(!sender_result.must_shutdown);
        assert_eq!(sender_result.time_from_server, 1234567890000);
    }

    fn build_event_sender() -> ReqwestEventSender {
        let http = HashMap::new();
        let url = format!("{}/bulk", &mockito::server_url());
        let url = url::Url::parse(&url).expect("Failed parsing the mock server url");

        ReqwestEventSender::new(http, url, "sdk-key")
    }
}
