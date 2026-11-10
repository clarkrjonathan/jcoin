//! The relay node: a libp2p circuit relay (v2) plus a rendezvous point.
//!
//! It is the one service nodes behind NAT need: they reserve a slot on it, so
//! others can reach them through it, and they register under the JeffyCoin
//! namespace with its rendezvous service, so they can find each other. Hole
//! punching (DCUtR) runs between the two nodes over the relayed connection and,
//! when it succeeds, moves their traffic to a direct connection.
//!
//! The relay is chain-blind. It never speaks the JeffyCoin protocol, and the
//! circuits it carries are encrypted end to end between the two nodes (Noise),
//! so it sees only ciphertext and cannot impersonate a node.

use std::error::Error;
use std::time::Duration;

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, noise, ping, relay, rendezvous, tcp, yamux, Multiaddr};

use super::host::IDENTIFY_VERSION;

/// Relay limits. libp2p's defaults (2 minutes and 128 KiB per circuit) are
/// sized for hole-punch coordination only. JeffyCoin also uses the relay as a
/// lasting fallback for nodes that cannot hole punch (strict carrier-grade
/// NAT), so circuits last long and carry enough for chain sync. A circuit that
/// hits a limit closes, and the nodes reconnect through the relay.
pub fn relay_config() -> relay::Config {
    relay::Config {
        max_circuit_duration: Duration::from_secs(12 * 60 * 60),
        max_circuit_bytes: 1 << 30, // 1 GiB
        max_circuits: 256,
        max_circuits_per_peer: 16,
        ..Default::default()
    }
}

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    rendezvous: rendezvous::server::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

/// Run a relay until the process ends.
///
/// `listen` is where it accepts connections. `external` are its publicly
/// reachable addresses: the relay serves reservations only once it knows at
/// least one, and it hands them to nodes in their reservations.
pub async fn run_relay(
    keypair: identity::Keypair,
    listen: Vec<Multiaddr>,
    external: Vec<Multiaddr>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let local_id = keypair.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| RelayBehaviour {
            relay: relay::Behaviour::new(key.public().to_peer_id(), relay_config()),
            rendezvous: rendezvous::server::Behaviour::new(rendezvous::server::Config::default()),
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_VERSION.to_string(),
                key.public(),
            )),
            ping: ping::Behaviour::default(),
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();

    for addr in listen {
        swarm.listen_on(addr)?;
    }
    for addr in external {
        let addr = strip_peer(addr);
        println!("[relay] reachable at {}", addr.clone().with(Protocol::P2p(local_id)));
        swarm.add_external_address(addr);
    }
    println!("[relay] peer id {local_id}");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("[relay] listening on {}", address.with(Protocol::P2p(local_id)));
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(event)) => match event {
                relay::Event::ReservationReqAccepted { src_peer_id, renewed: false, .. } => {
                    println!("[relay] reservation for {src_peer_id}");
                }
                relay::Event::CircuitReqAccepted { src_peer_id, dst_peer_id } => {
                    println!("[relay] circuit {src_peer_id} -> {dst_peer_id}");
                }
                _ => {}
            },
            SwarmEvent::Behaviour(RelayBehaviourEvent::Rendezvous(
                rendezvous::server::Event::PeerRegistered { peer, .. },
            )) => {
                println!("[relay] rendezvous registration from {peer}");
            }
            _ => {}
        }
    }
}

fn strip_peer(mut addr: Multiaddr) -> Multiaddr {
    if matches!(addr.iter().last(), Some(Protocol::P2p(_))) {
        addr.pop();
    }
    addr
}
