/*
 * Copyright 2021 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

mod config;

use tracing::debug;

use self::quilkin::filters::firewall::v1alpha1 as proto;
use crate::filters::prelude::*;

pub use config::{Action, Config, PortRange, PortRangeError, Rule};

crate::include_proto!("quilkin.filters.firewall.v1alpha1");

/// Filter for allowing/blocking traffic by IP and port.
pub struct Firewall {
    on_read: Vec<Rule>,
    on_write: Vec<Rule>,
}

impl Firewall {
    fn new(config: Config) -> Self {
        Self {
            on_read: config.on_read,
            on_write: config.on_write,
        }
    }
}

impl StaticFilter for Firewall {
    const NAME: &'static str = "quilkin.filters.firewall.v1alpha1.Firewall";
    type Configuration = Config;
    type BinaryConfiguration = proto::Firewall;

    fn try_from_config(config: Option<Self::Configuration>) -> Result<Self, CreationError> {
        Self::ensure_config_exists(config).map(Firewall::new)
    }
}

#[async_trait::async_trait]
impl Filter for Firewall {
    #[cfg_attr(feature = "instrument", tracing::instrument(skip(self, ctx)))]
    async fn read(&self, ctx: &mut ReadContext) -> Result<(), FilterError> {
        for rule in &self.on_read {
            if rule.contains(ctx.source.to_socket_addr().await?) {
                return match rule.action {
                    Action::Allow => {
                        debug!(
                            action = "Allow",
                            event = "read",
                            source = ?ctx.source.to_string()
                        );
                        Ok(())
                    }
                    Action::Deny => {
                        debug!(action = "Deny", event = "read", source = ?ctx.source);
                        Err(FilterError::new(PacketDenied))
                    }
                };
            }
        }
        debug!(
            action = "default: Deny",
            event = "read",
            source = ?ctx.source.to_string()
        );
        Err(FilterError::new(PacketDenied))
    }

    #[cfg_attr(feature = "instrument", tracing::instrument(skip(self, ctx)))]
    async fn write(&self, ctx: &mut WriteContext) -> Result<(), FilterError> {
        for rule in &self.on_write {
            if rule.contains(ctx.source.to_socket_addr().await?) {
                return match rule.action {
                    Action::Allow => {
                        debug!(
                            action = "Allow",
                            event = "write",
                            source = ?ctx.source.to_string()
                        );
                        Ok(())
                    }
                    Action::Deny => {
                        debug!(action = "Deny", event = "write", source = ?ctx.source);
                        Err(FilterError::new(PacketDenied))
                    }
                };
            }
        }

        debug!(
            action = "default: Deny",
            event = "write",
            source = ?ctx.source.to_string()
        );
        Err(FilterError::new(PacketDenied))
    }
}

#[derive(thiserror::Error, Debug)]
#[error("packet denied")]
pub struct PacketDenied;

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use crate::endpoint::Endpoint;
    use crate::filters::firewall::config::PortRange;
    use tracing_test::traced_test;

    use super::*;

    #[tokio::test]
    #[traced_test]
    async fn read() {
        let firewall = Firewall {
            on_read: vec![Rule {
                action: Action::Allow,
                source: "192.168.75.0/24".parse().unwrap(),
                ports: vec![PortRange::new(10, 100).unwrap()],
            }],
            on_write: vec![],
        };

        let local_ip = [192, 168, 75, 20];
        let mut ctx = ReadContext::new(
            vec![Endpoint::new((Ipv4Addr::LOCALHOST, 8080).into())],
            (local_ip, 80).into(),
            vec![],
        );
        assert!(firewall.read(&mut ctx).await.is_ok());

        let mut ctx = ReadContext::new(
            vec![Endpoint::new((Ipv4Addr::LOCALHOST, 8080).into())],
            (local_ip, 2000).into(),
            vec![],
        );
        assert!(logs_contain("quilkin::filters::firewall")); // the given name to the the logger by tracing
        assert!(logs_contain("Allow"));

        assert!(firewall.read(&mut ctx).await.is_err());
    }

    #[tokio::test]
    async fn write() {
        let firewall = Firewall {
            on_read: vec![],
            on_write: vec![Rule {
                action: Action::Allow,
                source: "192.168.75.0/24".parse().unwrap(),
                ports: vec![PortRange::new(10, 100).unwrap()],
            }],
        };

        let endpoint = Endpoint::new((Ipv4Addr::LOCALHOST, 80).into());
        let local_addr: crate::endpoint::EndpointAddress = (Ipv4Addr::LOCALHOST, 8081).into();

        let mut ctx = WriteContext::new(
            endpoint.clone(),
            ([192, 168, 75, 20], 80).into(),
            local_addr.clone(),
            vec![],
        );
        assert!(firewall.write(&mut ctx).await.is_ok());

        let mut ctx = WriteContext::new(
            endpoint,
            ([192, 168, 77, 20], 80).into(),
            local_addr,
            vec![],
        );
        assert!(firewall.write(&mut ctx).await.is_err());
    }
}
