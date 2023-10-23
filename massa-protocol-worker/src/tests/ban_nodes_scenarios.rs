// Copyright (c) 2022 MASSA LABS <info@massa.net>

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use massa_consensus_exports::MockConsensusController;
use massa_models::{block_id::BlockId, prehash::PreHashSet, slot::Slot};
use massa_pool_exports::MockPoolController;
use massa_pos_exports::MockSelectorController;
use massa_protocol_exports::PeerId;
use massa_protocol_exports::{test_exports::tools, ProtocolConfig};
use massa_signature::KeyPair;
use massa_test_framework::{Breakpoint, TestUniverse};
use massa_time::MassaTime;
use mockall::predicate;

use crate::handlers::peer_handler::models::{PeerInfo, PeerState};
use crate::wrap_network::ActiveConnectionsTrait;
use crate::wrap_network::MockActiveConnectionsTraitWrapper;
use crate::{
    handlers::{
        block_handler::{BlockInfoReply, BlockMessage},
        operation_handler::OperationMessage,
    },
    messages::Message,
};

use super::universe::{ProtocolForeignControllers, ProtocolTestUniverse};
use super::{context::protocol_test, tools::assert_hash_asked_to_node};

#[test]
fn test_protocol_bans_node_sending_block_header_with_invalid_signature() {
    let protocol_config = ProtocolConfig {
        thread_count: 2,
        unban_everyone_timer: MassaTime::from_millis(1000),
        ..Default::default()
    };

    let mut foreign_controllers = ProtocolForeignControllers::new_with_mocks();

    let block_creator = KeyPair::generate(0).unwrap();
    let block = ProtocolTestUniverse::create_block(&block_creator);
    let mut block_bad_public_key = block.clone();
    block_bad_public_key.content.header.content_creator_pub_key =
        KeyPair::generate(0).unwrap().get_public_key();
    let node_a_keypair = KeyPair::generate(0).unwrap();
    let node_a_peer_id = PeerId::from_public_key(node_a_keypair.get_public_key());

    let ban_breakpoint = Breakpoint::new();
    let ban_breakpoint_trigger_handle = ban_breakpoint.get_trigger_handle();
    let unban_breakpoint = Breakpoint::new();
    let unban_breakpoint_trigger_handle = unban_breakpoint.get_trigger_handle();

    foreign_controllers
        .peer_db
        .write()
        .expect_get_peers_mut()
        .times(1)
        .returning(move || {
            let mut peers = HashMap::new();
            peers.insert(
                node_a_peer_id,
                PeerInfo {
                    last_announce: None,
                    state: PeerState::Trusted,
                },
            );
            peers
        });
    foreign_controllers
        .peer_db
        .write()
        .expect_ban_peer()
        .returning(move |peer_id| {
            assert_eq!(peer_id, &node_a_peer_id);
            ban_breakpoint_trigger_handle.trigger();
        });
    foreign_controllers
        .peer_db
        .write()
        .expect_get_peers_in_test()
        .return_const(HashSet::default());
    foreign_controllers
        .peer_db
        .write()
        .expect_get_oldest_peer()
        .return_const(None);
    foreign_controllers
        .peer_db
        .write()
        .expect_get_rand_peers_to_send()
        .return_const(vec![]);
    foreign_controllers
        .peer_db
        .write()
        .expect_unban_peer()
        .returning(move |peer_id| {
            assert_eq!(peer_id, &node_a_peer_id);
            unban_breakpoint_trigger_handle.trigger();
        });
    let mut peers = HashMap::new();
    peers.insert(
        node_a_peer_id,
        PeerInfo {
            last_announce: None,
            state: PeerState::Banned,
        },
    );
    foreign_controllers
        .peer_db
        .write()
        .expect_get_peers()
        .return_const(peers);
    foreign_controllers
        .consensus_controller
        .expect_register_block_header()
        .return_once(move |block_id, header| {
            assert_eq!(block_id, block.id);
            assert_eq!(header.id, block.content.header.id);
        });
    let mut shared_active_connections = MockActiveConnectionsTraitWrapper::new();
    shared_active_connections.set_expectations(|active_connections| {
        active_connections
            .expect_get_peer_ids_connected()
            .returning(move || {
                let mut peers = HashSet::new();
                peers.insert(node_a_peer_id);
                peers
            });
        active_connections
            .expect_shutdown_connection()
            .times(1)
            .with(predicate::eq(node_a_peer_id))
            .returning(move |_| {});
    });
    foreign_controllers
        .network_controller
        .expect_get_active_connections()
        .returning(move || Box::new(shared_active_connections.clone()));

    let universe = ProtocolTestUniverse::new(foreign_controllers, protocol_config);

    universe.mock_message_receive(
        &node_a_peer_id,
        Message::Block(Box::new(BlockMessage::Header(
            block_bad_public_key.content.header.clone(),
        ))),
    );
    ban_breakpoint.wait();
    unban_breakpoint.wait();
}

#[test]
fn test_protocol_bans_node_sending_operation_with_invalid_signature() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    let protocol_config = ProtocolConfig {
        thread_count: 2,
        initial_peers: "./src/tests/empty_initial_peers.json".to_string().into(),
        ..Default::default()
    };
    let mut consensus_controller = Box::new(MockConsensusController::new());
    consensus_controller
        .expect_clone_box()
        .returning(|| Box::new(MockConsensusController::new()));
    let mut pool_controller = Box::new(MockPoolController::new());
    pool_controller
        .expect_clone_box()
        .returning(|| Box::new(MockPoolController::new()));
    let mut selector_controller = Box::new(MockSelectorController::new());
    selector_controller
        .expect_clone_box()
        .returning(|| Box::new(MockSelectorController::new()));
    protocol_test(
        &protocol_config,
        consensus_controller,
        pool_controller,
        selector_controller,
        move |mut network_controller, _storage, _protocol_controller| {
            //1. Create 1 node
            let node_a_keypair = KeyPair::generate(0).unwrap();
            let (node_a_peer_id, _node_a) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_a_keypair.get_public_key()));

            //2. Create a operation with bad public key.
            let mut operation = tools::create_operation_with_expire_period(&node_a_keypair, 1);
            operation.content_creator_pub_key = KeyPair::generate(0).unwrap().get_public_key();
            //end setup

            //3. Send operation to protocol.
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Operation(OperationMessage::Operations(vec![operation])),
                )
                .unwrap();

            std::thread::sleep(std::time::Duration::from_millis(1000));
            //4. Check that node connection is closed (node should be banned)
            assert_eq!(
                network_controller
                    .get_connections()
                    .get_peer_ids_connected()
                    .len(),
                0
            );
        },
    )
}

#[test]
fn test_protocol_bans_node_sending_header_with_invalid_signature() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    let protocol_config = ProtocolConfig {
        thread_count: 2,
        initial_peers: "./src/tests/empty_initial_peers.json".to_string().into(),
        ..Default::default()
    };

    let block_creator = KeyPair::generate(0).unwrap();
    let operation_1 = tools::create_operation_with_expire_period(&block_creator, 1);
    let block =
        tools::create_block_with_operations(&block_creator, Slot::new(1, 1), vec![operation_1]);
    let operation_2 = tools::create_operation_with_expire_period(&block_creator, 1);
    let block_2 = tools::create_block_with_operations(
        &block_creator,
        Slot::new(1, 1),
        vec![operation_2.clone()],
    );
    let mut consensus_controller = Box::new(MockConsensusController::new());
    consensus_controller
        .expect_clone_box()
        .returning(move || Box::new(MockConsensusController::new()));
    consensus_controller
        .expect_register_block_header()
        .return_once(move |block_id, header| {
            assert_eq!(block_id, block.id);
            assert_eq!(header.id, block.content.header.id);
        });

    let mut pool_controller = Box::new(MockPoolController::new());
    pool_controller
        .expect_clone_box()
        .returning(|| Box::new(MockPoolController::new()));
    let mut selector_controller = Box::new(MockSelectorController::new());
    selector_controller
        .expect_clone_box()
        .returning(|| Box::new(MockSelectorController::new()));
    protocol_test(
        &protocol_config,
        consensus_controller,
        pool_controller,
        selector_controller,
        move |mut network_controller, _storage, protocol_controller| {
            //1. Create 1 node
            let node_a_keypair = KeyPair::generate(0).unwrap();
            let (node_a_peer_id, node_a) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_a_keypair.get_public_key()));

            //2. Node A send the block
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block.content.header.clone()))),
                )
                .unwrap();

            // 3. Send wishlist
            protocol_controller
                .send_wishlist_delta(
                    vec![(block.id, Some(block.content.header.clone()))]
                        .into_iter()
                        .collect(),
                    PreHashSet::<BlockId>::default(),
                )
                .unwrap();

            assert_hash_asked_to_node(&node_a, &block.id);

            //4. Node A sends block info with bad ops list
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::DataResponse {
                        block_id: block.id,
                        block_info: BlockInfoReply::OperationIds(vec![operation_2.id]),
                    })),
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1000));
            //5. Check that node connection is closed (node should be banned)
            assert_eq!(
                network_controller
                    .get_connections()
                    .get_peer_ids_connected()
                    .len(),
                0
            );

            //6. Node A tries to send it
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block_2.content.header))),
                )
                .expect_err("Node A should not be able to send a block");
            std::thread::sleep(std::time::Duration::from_millis(1000));
        },
    )
}

#[test]
fn test_protocol_does_not_asks_for_block_from_banned_node_who_propagated_header() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    let protocol_config = ProtocolConfig {
        thread_count: 2,
        initial_peers: "./src/tests/empty_initial_peers.json".to_string().into(),
        ..Default::default()
    };
    let block_creator = KeyPair::generate(0).unwrap();
    let block = tools::create_block(&block_creator);
    let mut consensus_controller = Box::new(MockConsensusController::new());
    consensus_controller
        .expect_clone_box()
        .returning(|| Box::new(MockConsensusController::new()));
    consensus_controller
        .expect_register_block_header()
        .return_once(move |block_id, header| {
            assert_eq!(block_id, block.id);
            assert_eq!(header.id, block.content.header.id);
        });
    let mut pool_controller = Box::new(MockPoolController::new());
    pool_controller
        .expect_clone_box()
        .returning(|| Box::new(MockPoolController::new()));
    let mut selector_controller = Box::new(MockSelectorController::new());
    selector_controller
        .expect_clone_box()
        .returning(|| Box::new(MockSelectorController::new()));
    protocol_test(
        &protocol_config,
        consensus_controller,
        pool_controller,
        selector_controller,
        move |mut network_controller, _storage, protocol_controller| {
            //1. Create 1 node
            let node_a_keypair = KeyPair::generate(0).unwrap();
            let (node_a_peer_id, node_a) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_a_keypair.get_public_key()));

            //2. Send header to protocol.
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block.content.header.clone()))),
                )
                .unwrap();

            let expected_hash = block.id;
            //3. Get node A banned
            // New keypair to avoid getting same block id
            let keypair = KeyPair::generate(0).unwrap();
            let mut block = tools::create_block(&keypair);
            block.content.header.content_creator_pub_key =
                KeyPair::generate(0).unwrap().get_public_key();
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block.content.header.clone()))),
                )
                .unwrap();
            //4. Check that node connection is closed (node should be banned)
            std::thread::sleep(std::time::Duration::from_millis(1000));
            assert_eq!(
                network_controller
                    .get_connections()
                    .get_peer_ids_connected()
                    .len(),
                0
            );
            //5. Send a wishlist that ask for the first block
            protocol_controller
                .send_wishlist_delta(
                    vec![(expected_hash, Some(block.content.header))]
                        .into_iter()
                        .collect(),
                    PreHashSet::<BlockId>::default(),
                )
                .unwrap();
            let _ = node_a
                .recv_timeout(Duration::from_millis(1000))
                .expect_err("Node A should not receive a ask block");
        },
    )
}

#[test]
fn test_protocol_bans_all_nodes_propagating_an_attack_attempt() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    let protocol_config = ProtocolConfig {
        thread_count: 2,
        initial_peers: "./src/tests/empty_initial_peers.json".to_string().into(),
        ..Default::default()
    };
    let block_creator = KeyPair::generate(0).unwrap();
    let block = tools::create_block(&block_creator);
    let mut consensus_controller = Box::new(MockConsensusController::new());
    consensus_controller
        .expect_clone_box()
        .returning(|| Box::new(MockConsensusController::new()));
    consensus_controller
        .expect_register_block_header()
        .return_once(move |block_id, header| {
            assert_eq!(block_id, block.id);
            assert_eq!(header.id, block.content.header.id);
        });
    let mut pool_controller = Box::new(MockPoolController::new());
    pool_controller
        .expect_clone_box()
        .returning(|| Box::new(MockPoolController::new()));
    let mut selector_controller = Box::new(MockSelectorController::new());
    selector_controller
        .expect_clone_box()
        .returning(|| Box::new(MockSelectorController::new()));
    protocol_test(
        &protocol_config,
        consensus_controller,
        pool_controller,
        selector_controller,
        move |mut network_controller, _storage, protocol_controller| {
            //1. Create 2 nodes
            let node_a_keypair = KeyPair::generate(0).unwrap();
            let node_b_keypair = KeyPair::generate(0).unwrap();
            let (node_a_peer_id, _node_a) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_a_keypair.get_public_key()));
            let (node_b_peer_id, _node_b) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_b_keypair.get_public_key()));

            //end setup

            //2. Send header to protocol from the two nodes.
            network_controller
                .send_from_peer(
                    &node_a_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block.content.header.clone()))),
                )
                .unwrap();

            network_controller
                .send_from_peer(
                    &node_b_peer_id,
                    Message::Block(Box::new(BlockMessage::Header(block.content.header.clone()))),
                )
                .unwrap();

            //3. Connect a new node that is not involved in the attack.
            let node_c_keypair = KeyPair::generate(0).unwrap();
            let (node_c_peer_id, _node_c) = network_controller
                .create_fake_connection(PeerId::from_public_key(node_c_keypair.get_public_key()));

            //4. Notify protocol of the attack
            std::thread::sleep(std::time::Duration::from_millis(1000));
            protocol_controller.notify_block_attack(block.id).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1000));

            //5. Check all nodes are banned except node C.
            assert_eq!(
                network_controller
                    .get_connections()
                    .get_peer_ids_connected(),
                [node_c_peer_id].into_iter().collect::<HashSet<PeerId>>()
            );
        },
    )
}
