use crate::feature_requester::FeatureRequester;
use crate::feature_requester::ReqwestFeatureRequester;
use crate::LAUNCHDARKLY_TAGS_HEADER;
use std::collections::HashMap;
use thiserror::Error;
use url;

/// Error type used to represent failures when building a [FeatureRequesterFactory] instance.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum BuildError {
    /// Error used when a configuration setting is invalid.
    #[error("feature requester factory failed to build: {0}")]
    InvalidConfig(String),
}

/// Trait which allows creation of feature requesters.
///
/// Feature requesters are used by the polling data source (see [crate::PollingDataSourceBuilder])
/// to retrieve state information from an external resource such as the LaunchDarkly API.
pub trait FeatureRequesterFactory: Send {
    /// Create an instance of FeatureRequester.
    fn build(&self, tags: Option<String>) -> Result<Box<dyn FeatureRequester>, BuildError>;
}

pub struct ReqwestFeatureRequesterBuilder {
    url: String,
    sdk_key: String,
}

impl ReqwestFeatureRequesterBuilder {
    pub fn new(url: &str, sdk_key: &str) -> Self {
        Self {
            url: url.into(),
            sdk_key: sdk_key.into(),
        }
    }
}

impl FeatureRequesterFactory for ReqwestFeatureRequesterBuilder {
    fn build(&self, tags: Option<String>) -> Result<Box<dyn FeatureRequester>, BuildError> {
        let url = format!("{}/sdk/latest-all", self.url);
        let url = url::Url::parse(&url)
            .map_err(|_| BuildError::InvalidConfig("Invalid base url provided".into()))?;

        let headers = if let Some(tags) = tags {
            let mut headers = HashMap::new();
            headers.insert(LAUNCHDARKLY_TAGS_HEADER.to_string(), tags);
            headers
        } else {
            HashMap::new()
        };

        Ok(Box::new(ReqwestFeatureRequester::new(
            headers,
            url,
            self.sdk_key.clone(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_handles_url_parsing_failure() {
        let builder =
            ReqwestFeatureRequesterBuilder::new("This is clearly not a valid URL", "sdk-key");
        let result = builder.build(None);

        match result {
            Err(BuildError::InvalidConfig(_)) => (),
            _ => panic!("Build did not return the right type of error"),
        };
    }
}
