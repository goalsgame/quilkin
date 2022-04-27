/*
 * Copyright 2020 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

mod affix;
mod config;
mod metrics;
mod regex;

crate::include_proto!("quilkin.filters.capture.v1alpha1");

use std::sync::Arc;

use crate::{filters::prelude::*, metadata::Value};

use self::{
    affix::{Prefix, Suffix},
    metrics::Metrics,
    regex::Regex,
};

use self::quilkin::filters::capture::v1alpha1 as proto;

pub use config::{Config, Strategy};

/// Trait to implement different strategies for capturing packet data.
pub trait CaptureStrategy {
    /// Capture packet data from the contents, and optionally returns a value if
    /// anything was captured.
    fn capture(&self, contents: &mut Vec<u8>, metrics: &Metrics) -> Option<Value>;
}

pub struct Capture {
    capture: Box<dyn CaptureStrategy + Sync + Send>,
    /// metrics reporter for this filter.
    metrics: Metrics,
    metadata_key: Arc<String>,
    is_present_key: Arc<String>,
}

impl Capture {
    fn new(config: Config, metrics: Metrics) -> Self {
        Self {
            capture: config.strategy.into_capture(),
            metrics,
            is_present_key: Arc::new(config.metadata_key.clone() + "/is_present"),
            metadata_key: Arc::new(config.metadata_key),
        }
    }
}

impl Filter for Capture {
    #[cfg_attr(feature = "instrument", tracing::instrument(skip(self, ctx)))]
    fn read(&self, mut ctx: ReadContext) -> Option<ReadResponse> {
        let capture = self.capture.capture(&mut ctx.contents, &self.metrics);
        ctx.metadata
            .insert(self.is_present_key.clone(), Value::Bool(capture.is_some()));

        if let Some(value) = capture {
            ctx.metadata.insert(self.metadata_key.clone(), value);
            Some(ctx.into())
        } else {
            None
        }
    }
}

impl StaticFilter for Capture {
    const NAME: &'static str = "quilkin.filters.capture.v1alpha1.Capture";
    type Configuration = Config;
    type BinaryConfiguration = proto::Capture;

    fn try_from_config(config: Option<Self::Configuration>) -> Result<Self, Error> {
        Ok(Capture::new(
            Self::ensure_config_exists(config)?,
            Metrics::new()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        endpoint::{Endpoint, Endpoints},
        filters::metadata::CAPTURED_BYTES,
        metadata::Value,
        test_utils::assert_write_no_change,
    };

    use super::*;

    const TOKEN_KEY: &str = "TOKEN";

    #[test]
    fn factory_valid_config_all() {
        let config = serde_json::json!({
            "metadataKey": TOKEN_KEY.to_string(),
            "suffix": {
                "size": 3_i64,
                "remove": true,
            }
        });
        let filter = Capture::from_config(Some(serde_json::from_value(config).unwrap()));
        assert_end_strategy(&filter, TOKEN_KEY, true);
    }

    #[test]
    fn factory_valid_config_defaults() {
        let config = serde_json::json!({
            "suffix": {
                "size": 3_i64,
            }
        });

        let filter = Capture::from_config(Some(serde_json::from_value(config).unwrap()));
        assert_end_strategy(&filter, CAPTURED_BYTES, false);
    }

    #[test]
    fn invalid_config() {
        let config = serde_json::json!({
            "suffix": {
                "size": "WRONG",
            }
        });
        assert!(serde_json::from_value::<Config>(config).is_err());
    }

    #[test]
    fn read() {
        let config = Config {
            metadata_key: TOKEN_KEY.into(),
            strategy: Strategy::Suffix(Suffix {
                size: 3,
                remove: true,
            }),
        };

        let filter = Capture::from_config(config.into());
        assert_end_strategy(&filter, TOKEN_KEY, true);
    }

    #[test]
    fn read_overflow_capture_size() {
        let config = Config {
            metadata_key: TOKEN_KEY.into(),
            strategy: Strategy::Suffix(Suffix {
                size: 99,
                remove: true,
            }),
        };
        let filter = Capture::from_config(config.into());
        let endpoints = vec![Endpoint::new("127.0.0.1:81".parse().unwrap())];
        let response = filter.read(ReadContext::new(
            Endpoints::new(endpoints).into(),
            (std::net::Ipv4Addr::LOCALHOST, 80).into(),
            "abc".to_string().into_bytes(),
        ));

        assert!(response.is_none());
        let count = filter.metrics.packets_dropped_total.get();
        assert_eq!(1, count);
    }

    #[test]
    fn write() {
        let config = Config {
            strategy: Strategy::Suffix(Suffix {
                size: 0,
                remove: false,
            }),
            metadata_key: TOKEN_KEY.into(),
        };
        let filter = Capture::from_config(config.into());
        assert_write_no_change(&filter);
    }

    #[test]
    fn regex_capture() {
        let metrics = Metrics::new().unwrap();
        let end = Regex {
            pattern: ::regex::bytes::Regex::new(".{3}$").unwrap(),
        };
        let mut contents = b"helloabc".to_vec();
        let result = end.capture(&mut contents, &metrics).unwrap();
        assert_eq!(Value::Bytes(b"abc".to_vec().into()), result);
        assert_eq!(b"helloabc".to_vec(), contents);
    }

    #[test]
    fn end_capture() {
        let metrics = Metrics::new().unwrap();
        let mut end = Suffix {
            size: 3,
            remove: false,
        };
        let mut contents = b"helloabc".to_vec();
        let result = end.capture(&mut contents, &metrics).unwrap();
        assert_eq!(Value::Bytes(b"abc".to_vec().into()), result);
        assert_eq!(b"helloabc".to_vec(), contents);

        end.remove = true;

        let result = end.capture(&mut contents, &metrics).unwrap();
        assert_eq!(Value::Bytes(b"abc".to_vec().into()), result);
        assert_eq!(b"hello".to_vec(), contents);
    }

    #[test]
    fn beginning_capture() {
        let metrics = Metrics::new().unwrap();
        let mut beg = Prefix {
            size: 3,
            remove: false,
        };
        let mut contents = b"abchello".to_vec();

        let result = beg.capture(&mut contents, &metrics);
        assert_eq!(Some(Value::Bytes(b"abc".to_vec().into())), result);
        assert_eq!(b"abchello".to_vec(), contents);

        beg.remove = true;

        let result = beg.capture(&mut contents, &metrics);
        assert_eq!(Some(Value::Bytes(b"abc".to_vec().into())), result);
        assert_eq!(b"hello".to_vec(), contents);
    }

    fn assert_end_strategy<F>(filter: &F, key: &str, remove: bool)
    where
        F: Filter + ?Sized,
    {
        let endpoints = vec![Endpoint::new("127.0.0.1:81".parse().unwrap())];
        let response = filter
            .read(ReadContext::new(
                Endpoints::new(endpoints).into(),
                "127.0.0.1:80".parse().unwrap(),
                "helloabc".to_string().into_bytes(),
            ))
            .unwrap();

        if remove {
            assert_eq!(b"hello".to_vec(), response.contents);
        } else {
            assert_eq!(b"helloabc".to_vec(), response.contents);
        }

        let token = response
            .metadata
            .get(&Arc::new(key.into()))
            .unwrap()
            .as_bytes()
            .unwrap();
        assert_eq!(b"abc", &**token);
    }
}