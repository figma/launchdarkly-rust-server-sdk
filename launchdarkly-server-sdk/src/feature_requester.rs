use crate::ureq::{is_http_error_recoverable, is_http_success};
use ureq;

use super::stores::store_types::AllData;
use launchdarkly_server_sdk_evaluation::{Flag, Segment};
use std::collections::HashMap;
use url;

#[derive(Debug, PartialEq)]
pub enum FeatureRequesterError {
    Temporary,
    Permanent,
}

#[derive(Clone, Debug)]
struct CachedEntry(AllData<Flag, Segment>, String);

pub trait FeatureRequester: Send {
    fn get_all(&mut self) -> Result<AllData<Flag, Segment>, FeatureRequesterError>;
}

pub struct ReqwestFeatureRequester {
    default_headers: HashMap<String, String>,
    url: url::Url,
    sdk_key: String,
    cache: Option<CachedEntry>,
}

impl ReqwestFeatureRequester {
    pub fn new(default_headers: HashMap<String, String>, url: url::Url, sdk_key: String) -> Self {
        Self {
            default_headers,
            url,
            sdk_key,
            cache: None,
        }
    }
}

impl FeatureRequester for ReqwestFeatureRequester {
    fn get_all(&mut self) -> Result<AllData<Flag, Segment>, FeatureRequesterError> {
        let mut request_builder = ureq::request_url("GET", &self.url.clone())
            .set("Content-Type", "application/json")
            .set("Authorization", &self.sdk_key.clone())
            .set("User-Agent", &crate::USER_AGENT);

        if let Some(cache) = &self.cache {
            request_builder = request_builder.set("If-None-Match", &cache.1.clone());
        }

        for (k, v) in self.default_headers.clone() {
            request_builder = request_builder.set(&k, &v);
        }

        let resp = request_builder.call();

        let response = match resp {
            Ok(response) => response,
            Err(ureq::Error::Status(_code, response)) => {
                error!(
                    "An error occurred while retrieving flag information {} (status: {})",
                    response.status_text(),
                    response.status()
                );
                if is_http_error_recoverable(response.status()) {
                    return Err(FeatureRequesterError::Temporary);
                } else {
                    return Err(FeatureRequesterError::Permanent);
                }
            }
            Err(ureq::Error::Transport(_t)) => {
                error!("A transport error occurred while retrieving flag information");
                return Err(FeatureRequesterError::Temporary);
            }
        };

        if response.status() == 304 && self.cache.is_some() {
            // 304 is NOT-MODIFIED.
            let cache = self.cache.clone().unwrap();
            debug!("Returning cached data. Etag: {}", cache.1);
            return Ok(cache.0);
        }

        let etag: String = response
            .header(&"ETag".to_string())
            .unwrap_or_default()
            .to_string();

        if is_http_success(response.status()) {
            let resp_json = response.into_json::<AllData<Flag, Segment>>();
            return match resp_json {
                Ok(all_data) => {
                    if !etag.is_empty() {
                        debug!("Caching data for future use with etag: {}", etag);
                        self.cache = Some(CachedEntry(all_data.clone(), etag));
                    }
                    Ok(all_data)
                }
                Err(e) => {
                    error!("An error occurred while parsing the json response: {}", e);
                    Err(FeatureRequesterError::Permanent)
                }
            };
        }

        error!(
            "An error occurred while retrieving flag information. (status: {})",
            response.status()
        );

        if !is_http_error_recoverable(response.status()) {
            return Err(FeatureRequesterError::Permanent);
        }

        Err(FeatureRequesterError::Temporary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::mock;
    use test_case::test_case;

    #[tokio::test(flavor = "multi_thread")]
    async fn updates_etag_as_appropriate() {
        let _initial_request = mock("GET", "/")
            .with_status(200)
            .with_header("etag", "INITIAL-TAG")
            .with_body(r#"{"flags": {}, "segments": {}}"#)
            .expect(1)
            .create();
        let _second_request = mock("GET", "/")
            .with_status(304)
            .match_header("If-None-Match", "INITIAL-TAG")
            .expect(1)
            .create();
        let _third_request = mock("GET", "/")
            .with_status(200)
            .match_header("If-None-Match", "INITIAL-TAG")
            .with_header("etag", "UPDATED-TAG")
            .with_body(r#"{"flags": {}, "segments": {}}"#)
            .create();

        let mut requester = build_feature_requester();
        let result = requester.get_all();

        assert!(result.is_ok());
        if let Some(cache) = &requester.cache {
            assert_eq!("INITIAL-TAG", cache.1);
        }

        let result = requester.get_all();
        assert!(result.is_ok());
        if let Some(cache) = &requester.cache {
            assert_eq!("INITIAL-TAG", cache.1);
        }

        let result = requester.get_all();
        assert!(result.is_ok());
        if let Some(cache) = &requester.cache {
            assert_eq!("UPDATED-TAG", cache.1);
        }
    }

    #[test_case(400, FeatureRequesterError::Temporary)]
    #[test_case(401, FeatureRequesterError::Permanent)]
    #[test_case(408, FeatureRequesterError::Temporary)]
    #[test_case(409, FeatureRequesterError::Permanent)]
    #[test_case(429, FeatureRequesterError::Temporary)]
    #[test_case(430, FeatureRequesterError::Permanent)]
    #[test_case(500, FeatureRequesterError::Temporary)]
    #[tokio::test(flavor = "multi_thread")]
    async fn correctly_determines_unrecoverable_errors(
        status: usize,
        error: FeatureRequesterError,
    ) {
        let _initial_request = mock("GET", "/").with_status(status).create();

        let mut requester = build_feature_requester();
        let result = requester.get_all();

        if let Err(err) = result {
            assert_eq!(err, error);
        } else {
            panic!("get_all returned the wrong response");
        }
    }

    fn build_feature_requester() -> ReqwestFeatureRequester {
        let headers = HashMap::new();
        let url =
            url::Url::parse(&mockito::server_url()).expect("Failed parsing the mock server url");

        ReqwestFeatureRequester::new(headers, url, "sdk-key".to_string())
    }
}
