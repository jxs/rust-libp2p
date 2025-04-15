// Copyright 2020 Sigma Prime Pty Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

mod broadcast {
    use std::collections::{BTreeSet, HashMap, HashSet};

    use libp2p_core::PeerId;
    use rand::{seq::IteratorRandom, thread_rng};

    use crate::{
        types::{PeerConnections, PeerKind},
        RawMessage, TopicHash,
    };

    /// A Broadcast strategy.
    pub trait Broadcast {
        fn publish(
            &mut self,
            topic_hash: &TopicHash,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &BTreeSet<PeerId>,
            fanout_peers: &mut BTreeSet<PeerId>,
            explicit_peers: &BTreeSet<PeerId>,
        ) -> HashSet<PeerId>;

        fn forward(
            &mut self,
            message: RawMessage,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &BTreeSet<PeerId>,
            explicit_peers: &BTreeSet<PeerId>,
            propagation_source: Option<&PeerId>,
            originating_peers: HashSet<PeerId>,
        ) -> HashSet<PeerId>;
    }

    /// The default `Broadcast` strategy.
    pub struct Default {
        mesh_n: usize,
    }

    impl Broadcast for Default {
        fn publish(
            &mut self,
            topic_hash: &TopicHash,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &BTreeSet<PeerId>,
            fanout_peers: &mut BTreeSet<PeerId>,
            explicit_peers: &BTreeSet<PeerId>,
        ) -> HashSet<PeerId> {
            let mut peers_on_topic = connected_peers
                .iter()
                .filter(|(_, p)| p.topics.contains(topic_hash))
                .map(|(peer_id, _)| peer_id)
                .peekable();

            if peers_on_topic.peek().is_none() {
                return HashSet::new();
            }

            // TODO: why are mesh peers a `BtreeSet`?
            let mut peers_to_publish = mesh_peers.iter().copied().collect::<HashSet<PeerId>>();

            // Check if we are subscribed to the topic.
            if !mesh_peers.is_empty() {
                // We have a mesh set. We want to make sure to publish to at least `mesh_n`
                // peers (if possible).
                let needed_extra_peers = self.mesh_n.saturating_sub(mesh_peers.len());
                if needed_extra_peers > 0 {
                    // We don't have `mesh_n` peers in our mesh, we will randomly select extras
                    // and publish to them.
                    let extra_peers = peers_on_topic
                        .clone()
                        .choose_multiple(&mut thread_rng(), needed_extra_peers);
                    peers_to_publish.extend(extra_peers);
                }
            } else {
                tracing::debug!(topic=%topic_hash, "Topic not in the mesh");
                if !fanout_peers.is_empty() {
                    peers_to_publish.extend(fanout_peers.iter());
                } else {
                    // We have no fanout peers, select mesh_n of them and add them to the fanout
                    let new_peers = peers_on_topic.choose_multiple(&mut thread_rng(), self.mesh_n);
                    tracing::debug!("Adding peers added to fanout: {:?}", new_peers);
                    fanout_peers.extend(new_peers.clone());
                    peers_to_publish.extend(new_peers);
                }
            }

            peers_to_publish.extend(explicit_peers);
            let floodsub_peers = connected_peers
                .iter()
                .filter(|(_, c)| c.kind == PeerKind::Floodsub)
                .map(|(peer_id, _)| peer_id)
                .peekable();

            peers_to_publish.extend(floodsub_peers);

            peers_to_publish
        }

        fn forward(
            &mut self,
            message: RawMessage,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &BTreeSet<PeerId>,
            explicit_peers: &BTreeSet<PeerId>,
            propagation_source: Option<&PeerId>,
            originating_peers: HashSet<PeerId>,
        ) -> HashSet<PeerId> {
            let mut peers_to_forward = HashSet::new();

            // Add explicit peers and floodsub peers
            for (peer_id, peer) in connected_peers {
                if Some(peer_id) != propagation_source
                    && !originating_peers.contains(peer_id)
                    && Some(peer_id) != message.source.as_ref()
                    && peer.topics.contains(&message.topic)
                    && (explicit_peers.contains(peer_id) || peer.kind == PeerKind::Floodsub)
                {
                    peers_to_forward.insert(*peer_id);
                }
            }

            for peer_id in mesh_peers {
                if Some(peer_id) != propagation_source
                    && !originating_peers.contains(peer_id)
                    && Some(peer_id) != message.source.as_ref()
                {
                    peers_to_forward.insert(*peer_id);
                }
            }

            peers_to_forward
        }
    }
}

/// A Scoring strategy.
pub trait Scoring {}

pub(crate) mod mesh {

    use std::collections::{BTreeSet, HashMap, HashSet};

    use libp2p_core::PeerId;
    use rand::{seq::IteratorRandom, thread_rng};

    use crate::{
        types::{Graft, PeerConnections, Prune, RpcOut},
        TopicHash,
    };
    /// A Mesh management strategy
    pub trait Mesh {
        /// Select which peers to be part of the mesh when subscribing a topic, and to send the respective `Graft` messages.
        fn join(
            &mut self,
            topic_hash: &TopicHash,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            fanout_peers: &mut BTreeSet<PeerId>,
            explicit_peers: &mut HashSet<PeerId>,
        ) -> HashSet<PeerId>;

        /// A `Subscribe` message was received, should we add the peer to the mesh and Graft it?
        fn subscribe(&mut self, peer_id: PeerId, mesh_peers: &BTreeSet<PeerId>) -> bool;

        /// A `Graft` message was received, should we add the peer to our mesh and `Graft` it back or `Prune` it?
        fn graft(
            &mut self,
            peer_id: PeerId,
            topic_hash: &TopicHash,
            mesh_peers: &BTreeSet<PeerId>,
            explicit_peers: &HashSet<PeerId>,
        ) -> bool;

        /// Periodic process that can be used by a mesh strategy to perform mesh and fanout maintenance.
        /// Returns a list of peers with the respective messages to send to each.
        fn heartbeat(
            &mut self,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &mut HashMap<TopicHash, BTreeSet<PeerId>>,
            fanout_peers: &mut HashMap<TopicHash, BTreeSet<PeerId>>,
            explicit_peers: &mut HashSet<PeerId>,
        ) -> Vec<(PeerId, RpcOut)>;
    }

    // The default gossipsub mesh strategy.
    pub struct Default {
        /// Target number of peers for the mesh network (D in the spec, default is 6).
        mesh_n: usize,
        /// Minimum number of peers in mesh network before adding more (D_lo in the spec, default is 5).
        mesh_n_low: usize,
        /// Maximum number of peers in mesh network before removing some (D_high in the spec, default
        /// is 12).
        mesh_n_high: usize,
        /// The backoff time in seconds before we allow to `Graft` again
        backoff: Option<u64>,
    }

    impl Mesh for Default {
        fn join(
            &mut self,
            topic_hash: &TopicHash,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            fanout_peers: &mut BTreeSet<PeerId>,
            explicit_peers: &mut HashSet<PeerId>,
        ) -> HashSet<PeerId> {
            let mut mesh_peers = HashSet::new();
            let add_peers = std::cmp::min(fanout_peers.len(), self.mesh_n);
            tracing::debug!(
                topic=%topic_hash,
                "JOIN: Adding {:?} peers from the fanout for topic",
                add_peers
            );
            mesh_peers.extend(fanout_peers.iter().take(add_peers));

            // If we get enough peers from the fanout return.
            if mesh_peers.len() >= self.mesh_n {
                return mesh_peers;
            }

            //TODO: Should we filter the peers score at this point?
            let remaining = self.mesh_n - mesh_peers.len();

            let remaining_peers = connected_peers
                .keys()
                .filter(|peer| !explicit_peers.contains(peer))
                .copied()
                .choose_multiple(&mut thread_rng(), remaining);

            mesh_peers.extend(remaining_peers);

            mesh_peers
        }

        fn subscribe(&mut self, peer: PeerId, mesh_peers: &BTreeSet<PeerId>) -> bool {
            // Why do we only check the lower bound?
            mesh_peers.len() < self.mesh_n_low && !mesh_peers.contains(&peer)
        }

        fn graft(
            &mut self,
            peer_id: PeerId,
            topic_hash: &TopicHash,
            mesh_peers: &BTreeSet<PeerId>,
            explicit_peers: &HashSet<PeerId>,
        ) -> bool {
            // If the peer is already on our mesh or is an explicit peer ignore.
            if explicit_peers.contains(&peer_id) {
                tracing::warn!(peer=%peer_id, "GRAFT: ignoring request from direct peer");
                return false;
            }

            if mesh_peers.contains(&peer_id) {
                tracing::debug!(
                    peer=%peer_id,
                    topic=%&topic_hash,
                    "GRAFT: Received graft for peer that is already in topic"
                );
                return false;
            }
            if mesh_peers.len() >= self.mesh_n_high {
                return false;
            }
            true
        }

        fn heartbeat(
            &mut self,
            connected_peers: &HashMap<PeerId, PeerConnections>,
            mesh_peers: &mut HashMap<TopicHash, BTreeSet<PeerId>>,
            fanout_peers: &mut HashMap<TopicHash, BTreeSet<PeerId>>,
            explicit_peers: &mut HashSet<PeerId>,
        ) -> Vec<(PeerId, RpcOut)> {
            let mut actions = vec![];
            // Mesh management.
            for (topic_hash, peers) in mesh_peers.iter_mut() {
                // too little peers - add enough.
                if peers.len() < self.mesh_n_low {
                    tracing::debug!(
                        topic=%topic_hash,
                        "HEARTBEAT: Mesh low. Topic contains: {} needs: {}",
                        peers.len(),
                        self.mesh_n_low
                    );
                    let needed = self.mesh_n - peers.len();
                    let needed_peers = connected_peers
                        .keys()
                        .filter(|peer| !explicit_peers.contains(peer))
                        .copied()
                        .choose_multiple(&mut thread_rng(), needed);

                    actions.extend(needed_peers.iter().map(|peer_id| {
                        (
                            *peer_id,
                            RpcOut::Graft(Graft {
                                topic_hash: topic_hash.clone(),
                            }),
                        )
                    }));
                    tracing::debug!("Updating mesh, new mesh: {:?}", needed_peers);
                    peers.extend(needed_peers);
                    continue;
                }
                // too many peers - remove excess.
                if peers.len() > self.mesh_n_high {
                    let excess = peers.len() - self.mesh_n;
                    let excess_peers = connected_peers
                        .keys()
                        .filter(|peer| !explicit_peers.contains(peer))
                        .copied()
                        .choose_multiple(&mut thread_rng(), excess);
                    for peer_id in excess_peers {
                        actions.push((
                            peer_id,
                            RpcOut::Prune(Prune {
                                topic_hash: topic_hash.clone(),
                                peers: vec![],
                                backoff: self.backoff,
                            }),
                        ));
                        peers.remove(&peer_id);
                    }
                }
            }

            // Fanout management.
            for (topic_hash, peers) in fanout_peers.iter_mut() {
                // Remove disconnected and peers that are no longer subscribed to the topic.
                peers.retain(|peer_id| {
                    let Some(peer) = connected_peers.get(peer_id) else {
                        return false;
                    };
                    if !peer.topics.contains(topic_hash) {
                        return false;
                    }
                    true
                });

                // not enough peers, get enough.
                if peers.len() < self.mesh_n {}
            }

            actions
        }
    }
}
