/*
 * Copyright 2020 Google LLC All Rights Reserved.
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

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::from_utf8;
use std::sync::Arc;

use slog::{debug, error, info, o, Logger};
use tokio::io::Result;
use tokio::net::udp::{RecvHalf, SendHalf};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::{Mutex, RwLock};

use crate::config::{Config, ConnectionConfig};

type SessionMap = Arc<RwLock<HashMap<(SocketAddr, SocketAddr), Mutex<Session>>>>;

/// Server is the UDP server main implementation
pub struct Server {
    log: Logger,
}

impl Server {
    pub fn new(base: Logger) -> Self {
        let log = base.new(o!("source" => "server::Server"));
        return Server { log };
    }

    // TODO: write tests for this
    /// start the async processing of incoming UDP packets
    pub async fn run(self, config: Arc<Config>) -> Result<()> {
        self.log_config(&config);

        let (mut receive_socket, send_socket) = Server::bind(&config).await?.split();
        // HashMap key is from,destination addresses as a tuple.
        let sessions: SessionMap = Arc::new(RwLock::new(HashMap::new()));
        let (send_packets, receive_packets) = channel::<Packet>(1024);

        let log = self.log.clone();
        tokio::spawn(async move {
            Server::process_receive_packet_channel(&log, send_socket, receive_packets).await;
            debug!(log, "Receiver closed");
        });

        loop {
            if let Err(err) = Server::process_receive_socket(
                self.log.clone(),
                config.clone(),
                &mut receive_socket,
                sessions.clone(),
                send_packets.clone(),
            )
            .await
            {
                error!(self.log, "Error processing reeive socket: {}", err);
            }
        }
    }

    // TODO: write tests
    /// process_receive_socket takes packets from the local socket and asynchronously
    /// processes them to send them out to endpoints.
    async fn process_receive_socket(
        log: Logger,
        config: Arc<Config>,
        receive_socket: &mut RecvHalf,
        sessions: SessionMap,
        send_packets: Sender<Packet>,
    ) -> Result<()> {
        let mut buf: Vec<u8> = vec![0; 65535];
        let (size, recv_addr) = receive_socket.recv_from(&mut buf).await?;
        tokio::spawn(async move {
            let endpoints = config.get_endpoints();
            let packet = &buf[..size];

            debug!(
                log,
                "Packet Received from: {}, {}",
                recv_addr,
                from_utf8(packet).unwrap()
            );

            for (_, dest) in endpoints.iter() {
                if let Err(err) = Server::ensure_session(
                    &log,
                    sessions.clone(),
                    recv_addr,
                    *dest,
                    send_packets.clone(),
                )
                .await
                {
                    error!(log, "Error ensuring session exists: {}", err);
                }

                let map = sessions.read().await;
                let mut session = map.get(&(recv_addr, *dest)).unwrap().lock().await;
                if let Err(err) = session.send_to(packet).await {
                    error!(log, "Error ensuring sending packet: {}", err)
                }
            }
        });
        return Ok(());
    }

    // TODO: write tests for this
    /// process_receive_packet_channel blocks on receive_packets.recv() channel
    /// and sends each packet on to the Packet.dest
    async fn process_receive_packet_channel(
        log: &Logger,
        mut send_socket: SendHalf,
        mut receive_packets: Receiver<Packet>,
    ) {
        while let Some(packet) = receive_packets.recv().await {
            // TODO: wrap this in tokio::spawn?
            debug!(
                log,
                "Sending packet back to origin {}, {}",
                packet.dest,
                String::from_utf8(packet.contents.clone()).unwrap()
            );

            if let Err(err) = send_socket
                .send_to(packet.contents.as_slice(), &packet.dest)
                .await
            {
                error!(log, "Error sending packet to {}, {}", packet.dest, err);
            }
        }
    }

    /// log_config outputs a log of what is configured
    fn log_config(&self, config: &Arc<Config>) {
        info!(self.log, "Starting on port {}", config.local.port);
        match &config.connections {
            ConnectionConfig::Sender { address, .. } => {
                info!(self.log, "Sender configuration"; "address" => address)
            }
            ConnectionConfig::Receiver { endpoints } => {
                info!(self.log, "Receiver configuration"; "endpoints" => endpoints.len())
            }
        };
    }

    /// bind binds the local configured port
    async fn bind(config: &Config) -> Result<UdpSocket> {
        let addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), config.local.port);
        return UdpSocket::bind(addr).await;
    }

    // TODO: write tests for this
    /// ensure_session makes sure there is a value session for the name in the sessions map
    async fn ensure_session(
        log: &Logger,
        sessions: SessionMap,
        from: SocketAddr,
        dest: SocketAddr,
        sender: Sender<Packet>,
    ) -> Result<()> {
        {
            let map = sessions.read().await;
            if map.contains_key(&(from, dest)) {
                return Ok(());
            }
        }
        let s = Session::new(log, from, dest, sender).await?;
        {
            let mut map = sessions.write().await;
            map.insert((from, dest), Mutex::new(s));
        }
        return Ok(());
    }
}

/// Packet represents a packet that needs to go somewhere
struct Packet {
    dest: SocketAddr,
    contents: Vec<u8>,
}

/// Session encapsulates a UDP stream session
struct Session {
    log: Logger,
    send: SendHalf,
    /// dest is where to send data to
    dest: SocketAddr,
    /// from is the original sender
    from: SocketAddr,
    // TODO: store a session expiry, and update when you send data
}

impl Session {
    // TODO: write some tests
    async fn new(
        base: &Logger,
        from: SocketAddr,
        dest: SocketAddr,
        sender: Sender<Packet>,
    ) -> Result<Self> {
        let addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0);
        let (recv, send) = UdpSocket::bind(addr).await?.split();
        let mut s = Session {
            log: base.new(o!("source" => "server::Session", "from" => from, "dest" => dest)),
            send,
            from,
            dest,
        };
        debug!(s.log, "Session created");

        s.run(recv, sender);

        return Ok(s);
    }

    // TODO: write some tests
    /// run starts processing receiving udp packets on its UdpSocket
    fn run(&mut self, mut recv: RecvHalf, mut sender: Sender<Packet>) {
        let log = self.log.clone();
        let from = self.from;
        tokio::spawn(async move {
            let mut buf: Vec<u8> = vec![0; 65535];
            // TODO: work out how to shut this down once this session expires
            loop {
                debug!(log, "awaiting incoming packet");
                match recv.recv_from(&mut buf).await {
                    Err(err) => error!(log, "Error receiving packet: {}", err),
                    Ok((size, recv_addr)) => {
                        let packet = &buf[..size];
                        debug!(
                            log,
                            "Received packet from {}, {}",
                            recv_addr,
                            from_utf8(packet).unwrap()
                        );
                        if let Err(err) = sender
                            .send(Packet {
                                contents: packet.to_vec(),
                                dest: from,
                            })
                            .await
                        {
                            println!("Error sending packet to channel: {}", err);
                        }
                    }
                }
            }
        });
    }

    // TODO: write some tests
    /// Both sends a packet to the Session's dest.
    async fn send_to(&mut self, buf: &[u8]) -> Result<usize> {
        debug!(
            self.log,
            "Sending packet to: {}, {}",
            self.dest,
            from_utf8(buf).unwrap()
        );
        return self.send.send_to(buf, &self.dest).await;
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use crate::config::{Config, ConnectionConfig, Local};
    use crate::server::Server;

    #[tokio::test]
    async fn bind() {
        let config = Config {
            local: Local { port: 12345 },
            connections: ConnectionConfig::Receiver {
                endpoints: Vec::new(),
            },
        };
        let socket = Server::bind(&config).await.unwrap();
        let addr = socket.local_addr().unwrap();

        let expected = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 12345);
        assert_eq!(expected, addr)
    }
}